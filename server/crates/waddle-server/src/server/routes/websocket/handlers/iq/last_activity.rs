use super::{
    build_iq_error_xml_typed, build_last_activity_response, chrono, forbidden_iq_error,
    internal_server_error_iq_error, iq_to_xml, local_account_exists, not_authorized_iq_error,
    service_unavailable_iq_error, warn, FullJid, WebSocketState,
};

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
        // XEP-0191 block state is enforced by the dispatch gate before this
        // handler runs.
        // XEP-0012: only entities authorized to view the user's presence may
        // learn availability or last activity; everyone else gets the same
        // answer as for an offline user (forbidden / service-unavailable).
        let requester_bare = sender_jid.to_bare();
        let authorized = requester_bare == target_bare
            || crate::server::routes::websocket::s2s_accounts::shares_presence_with(
                state,
                &target_bare,
                &requester_bare,
            )
            .await;

        if authorized {
            if !crate::server::routes::websocket::handlers::account_reads::available_resources(
                state,
                &target_bare,
            )
            .await
            .is_empty()
            {
                return vec![iq_to_xml(build_last_activity_response(iq, 0, None))];
            }

            let record = match state
                .deps
                .app_state
                .db_pool
                .global_actor()
                .ask(crate::db::last_activity::lookup(&target_bare))
                .await
            {
                Ok(row) => row
                    .map(|row| crate::db::last_activity::decode(&row))
                    .transpose()
                    .map_err(|error| error.to_string()),
                Err(error) => Err(error.to_string()),
            };
            return match record {
                Ok(Some((logged_out_at_ms, status))) => {
                    let seconds =
                        (chrono::Utc::now().timestamp_millis() - logged_out_at_ms).max(0) / 1000;
                    vec![iq_to_xml(build_last_activity_response(
                        iq,
                        seconds as u64,
                        status.as_deref(),
                    ))]
                }
                // The account has never logged out, so the server has no
                // last presence activity to report. The requester is
                // authorized, so this is not `forbidden`.
                Ok(None) => vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    service_unavailable_iq_error("No last activity recorded for this account."),
                )],
                Err(error) => {
                    warn!(error = %error, target = %target_bare, "Failed to read last activity");
                    vec![build_iq_error_xml_typed(
                        iq.id(),
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )]
                }
            };
        }

        let Some(node) = target_bare.node() else {
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                service_unavailable_iq_error("Service unavailable at this address."),
            )];
        };
        match local_account_exists(
            state.deps.app_state.db_pool.global_actor(),
            node.as_str(),
            domain,
        )
        .await
        {
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
