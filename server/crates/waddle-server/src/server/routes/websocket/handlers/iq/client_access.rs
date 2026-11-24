//! XEP-0494 Client Access Management: an account owner lists and revokes the
//! OAuth grants (XEP-0493) that give clients access to the account.

use super::{
    bad_request_iq_error, build_iq_error_xml_typed, forbidden_iq_error,
    internal_server_error_iq_error, iq_to_xml, item_not_found_iq_error, mark_span_error,
    not_authorized_iq_error, warn, FullJid, Session, WebSocketState,
};
use crate::auth::oauth_grants::{OAuthGrant, OAuthGrantStore};
use waddle_xmpp::xep::xep0494::{
    build_clients_result, build_revoke_result, parse_cam_request, CamClient, CamClientType,
    CamPermission, CamRequest,
};

fn to_cam_client(grant: &OAuthGrant) -> CamClient {
    let used = grant.last_used_at.is_some();
    CamClient {
        id: grant.grant_id.clone(),
        connected: grant.connected,
        client_type: if used || grant.connected {
            CamClientType::Session
        } else {
            CamClientType::Access
        },
        first_seen: grant.created_at,
        last_seen: grant.last_used_at.unwrap_or(grant.created_at),
        permission: CamPermission::Normal,
    }
}

pub(super) async fn handle_client_access_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    authenticated_session: Option<&Session>,
    response_from: Option<&str>,
    response_to: Option<&str>,
) -> Vec<String> {
    let error = |condition| {
        vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            condition,
        )]
    };

    let Some(sender_jid) = sender_jid else {
        return error(not_authorized_iq_error("Authentication required."));
    };
    let account = sender_jid.to_bare();
    // The listing is of the sender's own account.
    if iq
        .to()
        .is_some_and(|to| to.to_bare() != account || to.resource().is_some())
    {
        return error(forbidden_iq_error(
            "Client access is managed on the account's own address.",
        ));
    }
    let request = match parse_cam_request(iq) {
        Ok(request) => request,
        Err(_) => return error(bad_request_iq_error("Malformed IQ payload.")),
    };

    let store = OAuthGrantStore::new(&state.deps.auth_state.session_manager);

    // XEP-0493: a client granted `xmpp:client:normal` must not manage the
    // account's access.
    if let Some(session) = authenticated_session {
        match store.scopes_of_session(&session.id).await {
            Ok(Some(scopes)) if !scopes.iter().any(|scope| scope.manages_account_access()) => {
                return error(forbidden_iq_error(
                    "This client's authorization does not permit managing account access.",
                ));
            }
            Ok(_) => {}
            Err(err) => {
                mark_span_error("failed to read the client's authorization scope");
                warn!(error = %err, "Failed to read OAuth grant scope");
                return error(internal_server_error_iq_error("Internal server error."));
            }
        }
    }

    let user_jid = account.to_string();
    match request {
        CamRequest::List => match store.list(&user_jid).await {
            Ok(grants) => {
                let clients: Vec<CamClient> = grants.iter().map(to_cam_client).collect();
                vec![iq_to_xml(build_clients_result(iq, &clients))]
            }
            Err(err) => {
                mark_span_error("failed to list OAuth grants");
                warn!(error = %err, "Failed to list OAuth grants");
                error(internal_server_error_iq_error("Internal server error."))
            }
        },
        CamRequest::Revoke { id } => match store.revoke(&user_jid, &id).await {
            Ok(true) => vec![iq_to_xml(build_revoke_result(iq))],
            Ok(false) => error(item_not_found_iq_error("No such client.")),
            Err(err) => {
                mark_span_error("failed to revoke an OAuth grant");
                warn!(error = %err, "Failed to revoke OAuth grant");
                error(internal_server_error_iq_error("Internal server error."))
            }
        },
    }
}
