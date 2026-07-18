use super::transport_xml::{element_to_xml, sasl_failure_xml, sasl_success_xml};
use super::*;

/// Handle SASL OAUTHBEARER authentication.
pub(super) async fn handle_sasl_oauthbearer(
    b64_data: &str,
    state: &WebSocketState,
    authenticated_session: &mut Option<Session>,
    phase: &mut ConnectionPhase,
) -> Vec<String> {
    debug!("SASL OAUTHBEARER auth attempt");

    let decoded = match BASE64_STANDARD.decode(b64_data) {
        Ok(data) => data,
        Err(e) => {
            warn!(error = %e, "SASL OAUTHBEARER: failed to decode base64 data");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    let token = match parse_oauthbearer(&decoded) {
        Ok(OAuthBearerResult::Credentials(credentials)) => credentials.token,
        Ok(OAuthBearerResult::DiscoveryRequest) => {
            warn!("SASL OAUTHBEARER: discovery request received on token-auth WebSocket path");
            return vec![sasl_failure_xml("not-authorized")];
        }
        Err(e) => {
            warn!(error = %e, "SASL OAUTHBEARER: failed to parse bearer data");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    match state
        .deps
        .auth_state
        .session_manager
        .validate_session(&token)
        .await
    {
        Ok(session) => {
            let bare_jid_str =
                match localpart_to_jid(&session.xmpp_localpart, &state.deps.auth_state.xmpp_domain)
                {
                    Ok(jid) => jid,
                    Err(e) => {
                        warn!(
                            localpart = %session.xmpp_localpart,
                            error = %e,
                            "SASL OAUTHBEARER: failed to build JID from session localpart",
                        );
                        return vec![sasl_failure_xml("not-authorized")];
                    }
                };

            let full_jid = match format!("{}/pending", bare_jid_str).parse::<FullJid>() {
                Ok(jid) => jid,
                Err(e) => {
                    warn!(jid = %bare_jid_str, error = %e, "SASL OAUTHBEARER: JID construction failed");
                    return vec![sasl_failure_xml("not-authorized")];
                }
            };

            info!(
                jid = %bare_jid_str,
                user_jid = %session.user_jid,
                "SASL OAUTHBEARER authentication successful",
            );

            *authenticated_session = Some(session);
            *phase = ConnectionPhase::authenticated(&full_jid);

            vec![sasl_success_xml()]
        }
        Err(e) => {
            warn!(error = %e, "SASL OAUTHBEARER authentication failed");
            vec![sasl_failure_xml("not-authorized")]
        }
    }
}

/// Handle SASL SCRAM-SHA-256 client-first-message.
///
/// Parses the client-first to extract the username, looks up stored SCRAM
/// credentials, creates a ScramServer with the user's salt/iterations, and
/// returns a `<challenge>` frame.
pub(super) async fn handle_sasl_scram_client_first(
    b64_data: &str,
    domain: &str,
    state: &WebSocketState,
    phase: &mut ConnectionPhase,
) -> Vec<String> {
    debug!("SASL SCRAM-SHA-256 auth attempt");

    let decoded = match BASE64_STANDARD.decode(b64_data.trim()) {
        Ok(data) => data,
        Err(e) => {
            warn!(error = %e, "SCRAM: failed to decode base64 client-first");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    let client_first = match String::from_utf8(decoded) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "SCRAM: invalid UTF-8 in client-first");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    // Parse username from client-first-message: "n,,n=<username>,r=<nonce>"
    // Use a temporary ScramServer to extract it.
    let username = {
        let mut tmp = ScramServer::new();
        match tmp.process_client_first(&client_first) {
            Ok(result) => result.username,
            Err(e) => {
                warn!(error = %e, "SCRAM: failed to parse client-first");
                return vec![sasl_failure_xml("not-authorized")];
            }
        }
    };

    // Look up SCRAM credentials for the user
    let native_user_store =
        NativeUserStore::new(state.deps.app_state.db_pool.global_actor().clone());

    let creds = match native_user_store
        .get_scram_credentials(&username, domain)
        .await
    {
        Ok(Some(creds)) => creds,
        Ok(None) => {
            warn!(username = %username, "SCRAM: user not found");
            return vec![sasl_failure_xml("not-authorized")];
        }
        Err(e) => {
            warn!(error = %e, username = %username, "SCRAM: credential lookup failed");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    // Create ScramServer with the user's stored salt and iterations, then
    // process the client-first again to produce a challenge with the correct params.
    let mut scram_server = ScramServer::with_salt_b64(creds.salt_b64, creds.iterations);
    let server_first = match scram_server.process_client_first(&client_first) {
        Ok(result) => result,
        Err(e) => {
            warn!(error = %e, "SCRAM: failed to process client-first with stored params");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    let challenge_b64 = BASE64_STANDARD.encode(server_first.message.as_bytes());
    debug!(username = %username, "SCRAM-SHA-256 challenge generated");

    *phase = ConnectionPhase::scram_pending(ScramPendingState::new(
        scram_server,
        creds.stored_key,
        creds.server_key,
        username,
    ));

    vec![element_to_xml(
        Element::builder("challenge", waddle_xmpp::ns::SASL)
            .append(challenge_b64)
            .build(),
    )]
}

/// Handle SASL SCRAM-SHA-256 response (client-final-message).
///
/// Verifies the client proof against stored keys and returns `<success>` or
/// `<failure>`.
pub(super) fn handle_sasl_scram_response(
    b64_data: &str,
    domain: &str,
    mut scram: ScramPendingState,
    authenticated_session: &mut Option<Session>,
    phase: &mut ConnectionPhase,
) -> Vec<String> {
    let decoded = match BASE64_STANDARD.decode(b64_data.trim()) {
        Ok(data) => data,
        Err(e) => {
            warn!(error = %e, "SCRAM: failed to decode base64 client-final");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    let client_final = match String::from_utf8(decoded) {
        Ok(s) => s,
        Err(e) => {
            warn!(error = %e, "SCRAM: invalid UTF-8 in client-final");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    let server_final = match scram.process_client_final(&client_final) {
        Ok(result) => {
            waddle_xmpp::metrics::record_auth_attempt("SCRAM-SHA-256", true);
            result
        }
        Err(e) => {
            waddle_xmpp::metrics::record_auth_attempt("SCRAM-SHA-256", false);
            warn!(
                error = %e,
                username = %scram.username(),
                "SCRAM-SHA-256 authentication failed"
            );
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    // Authentication successful - create session
    let bare_jid_str = format!("{}@{}", scram.username(), domain);
    let full_jid = match format!("{}/pending", bare_jid_str).parse::<FullJid>() {
        Ok(jid) => jid,
        Err(e) => {
            warn!(error = %e, jid = %bare_jid_str, "SCRAM: JID construction failed");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    let bare_jid: BareJid = match bare_jid_str.parse() {
        Ok(jid) => jid,
        Err(e) => {
            warn!(error = %e, "SCRAM: bare JID parse failed");
            return vec![sasl_failure_xml("not-authorized")];
        }
    };

    info!(
        jid = %bare_jid_str,
        "SASL SCRAM-SHA-256 authentication successful",
    );

    let session = Session::new(&bare_jid.to_string(), scram.username(), scram.username());

    *authenticated_session = Some(session);
    *phase = ConnectionPhase::authenticated(&full_jid);

    let success_b64 = BASE64_STANDARD.encode(server_final.message.as_bytes());
    vec![element_to_xml(
        Element::builder("success", waddle_xmpp::ns::SASL)
            .append(success_b64)
            .build(),
    )]
}
