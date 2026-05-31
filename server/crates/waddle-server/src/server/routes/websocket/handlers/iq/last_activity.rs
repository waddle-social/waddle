use super::*;

pub(super) async fn handle_last_activity_iq(
    iq: &xmpp_parsers::iq::Iq,
    domain: &str,
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

    let Some(target) = iq.to() else {
        let response = build_last_activity_response(
            iq,
            state
                .deps
                .protocol
                .connection_registry
                .server_uptime_seconds(),
            None,
        );
        return vec![iq_to_xml(response)];
    };

    if target.node().is_none() && target.domain().as_str() == domain {
        let response = build_last_activity_response(
            iq,
            state
                .deps
                .protocol
                .connection_registry
                .server_uptime_seconds(),
            None,
        );
        return vec![iq_to_xml(response)];
    }

    if target.node().is_some() && target.resource().is_none() && target.domain().as_str() == domain
    {
        let target_bare = target.to_bare();
        let global_db = match state
            .deps
            .app_state
            .db_pool
            .global_actor()
            .clone()
            .ask(GetDatabase)
            .await
        {
            Ok(db) => db,
            Err(error) => {
                warn!(error = %error, "Failed to access database for last-activity block check");
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        };
        let blocking_storage = DatabaseBlockingStorage::new(global_db);
        match blocking_storage
            .is_blocked_jid(&target_bare, &Jid::from(sender_jid.clone()))
            .await
        {
            Ok(true) => {
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    service_unavailable_iq_error("Service unavailable at this address."),
                )];
            }
            Ok(false) => {}
            Err(error) => {
                warn!(error = %error, target = %target_bare, "Failed to check last-activity block state");
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        }

        if !state
            .deps
            .protocol
            .connection_registry
            .get_available_resources_for_user(&target_bare)
            .is_empty()
        {
            return vec![iq_to_xml(build_last_activity_response(iq, 0, None))];
        }

        if let Some(last_activity) = state
            .deps
            .protocol
            .connection_registry
            .get_last_activity(&target_bare)
        {
            let seconds = chrono::Utc::now()
                .signed_duration_since(last_activity.timestamp)
                .num_seconds()
                .max(0) as u64;
            let response =
                build_last_activity_response(iq, seconds, last_activity.status.as_deref());
            return vec![iq_to_xml(response)];
        }

        let Some(node) = target_bare.node() else {
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                service_unavailable_iq_error("Service unavailable at this address."),
            )];
        };
        let native_user_store =
            NativeUserStore::new(state.deps.app_state.db_pool.global_actor().clone());
        match native_user_store.user_exists(node.as_str(), domain).await {
            Ok(false) => {
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    service_unavailable_iq_error("Service unavailable at this address."),
                )];
            }
            Ok(true) => {
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    forbidden_iq_error("Operation not permitted."),
                )];
            }
            Err(error) => {
                warn!(error = %error, target = %target_bare, "Failed to check local user for last-activity query");
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
        }
    }

    vec![build_iq_error_xml_typed(
        iq.id(),
        response_from,
        response_to,
        service_unavailable_iq_error("Service unavailable at this address."),
    )]
}
