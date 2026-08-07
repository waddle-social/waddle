use super::transport_xml::{element_to_xml, sasl_failure_xml, sasl_success_xml};
use crate::server::routes::auth_telemetry::{
    record_auth_failure, record_auth_success, AuthFailure,
};
use waddle_xmpp::telemetry::attributes::AuthStage;

/// Count the failed attempt on `xmpp.auth.attempts` (#1320) and build
/// the SASL failure frame. Every failure return in this module routes
/// through here so early-step rejections (bad base64, unknown user,
/// malformed first message) are not invisible to the counter.
fn sasl_failure_counted(mechanism: &'static str) -> String {
    waddle_xmpp::metrics::record_auth_attempt(mechanism, false);
    sasl_failure_xml("not-authorized")
}

fn scram_bare_identifier(username: &str, domain: &str) -> Option<BareJid> {
    format!("{username}@{domain}").parse().ok()
}

pub(super) fn record_scram_failure(failure: AuthFailure<'_>, bare_identifier: Option<&BareJid>) {
    record_auth_failure("native", bare_identifier, failure);
    waddle_xmpp::metrics::record_auth_attempt("SCRAM-SHA-256", false);
}

fn scram_failure_counted(failure: AuthFailure<'_>, bare_identifier: Option<&BareJid>) -> String {
    record_scram_failure(failure, bare_identifier);
    sasl_failure_xml("not-authorized")
}
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
            return vec![sasl_failure_counted("OAUTHBEARER")];
        }
    };

    let token = match parse_oauthbearer(&decoded) {
        Ok(OAuthBearerResult::Credentials(credentials)) => credentials.token,
        Ok(OAuthBearerResult::DiscoveryRequest) => {
            warn!("SASL OAUTHBEARER: discovery request received on token-auth WebSocket path");
            return vec![sasl_failure_counted("OAUTHBEARER")];
        }
        Err(e) => {
            warn!(error = %e, "SASL OAUTHBEARER: failed to parse bearer data");
            return vec![sasl_failure_counted("OAUTHBEARER")];
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
                        return vec![sasl_failure_counted("OAUTHBEARER")];
                    }
                };

            let full_jid = match format!("{}/pending", bare_jid_str).parse::<FullJid>() {
                Ok(jid) => jid,
                Err(e) => {
                    warn!(jid = %bare_jid_str, error = %e, "SASL OAUTHBEARER: JID construction failed");
                    return vec![sasl_failure_counted("OAUTHBEARER")];
                }
            };

            info!(
                jid = %bare_jid_str,
                user_jid = %session.user_jid,
                "SASL OAUTHBEARER authentication successful",
            );

            *authenticated_session = Some(session);
            *phase = ConnectionPhase::authenticated(&full_jid);

            waddle_xmpp::metrics::record_auth_attempt("OAUTHBEARER", true);
            vec![sasl_success_xml()]
        }
        Err(e) => {
            warn!(error = %e, "SASL OAUTHBEARER authentication failed");
            vec![sasl_failure_counted("OAUTHBEARER")]
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
        Err(_) => {
            return vec![scram_failure_counted(AuthFailure::ScramMalformed, None)];
        }
    };

    let client_first = match String::from_utf8(decoded) {
        Ok(s) => s,
        Err(_) => {
            return vec![scram_failure_counted(AuthFailure::ScramMalformed, None)];
        }
    };

    // Parse username from client-first-message: "n,,n=<username>,r=<nonce>"
    // Use a temporary ScramServer to extract it.
    let username = {
        let mut tmp = ScramServer::new();
        match tmp.process_client_first(&client_first) {
            Ok(result) => result.username,
            Err(_) => {
                return vec![scram_failure_counted(AuthFailure::ScramMalformed, None)];
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
            let bare_identifier = scram_bare_identifier(&username, domain);
            return vec![scram_failure_counted(
                AuthFailure::ScramUnknownUser,
                bare_identifier.as_ref(),
            )];
        }
        Err(_) => {
            let bare_identifier = scram_bare_identifier(&username, domain);
            return vec![scram_failure_counted(
                AuthFailure::ScramOther,
                bare_identifier.as_ref(),
            )];
        }
    };

    // Create ScramServer with the user's stored salt and iterations, then
    // process the client-first again to produce a challenge with the correct params.
    let mut scram_server = ScramServer::with_salt_b64(creds.salt_b64, creds.iterations);
    let server_first = match scram_server.process_client_first(&client_first) {
        Ok(result) => result,
        Err(_) => {
            let bare_identifier = scram_bare_identifier(&username, domain);
            return vec![scram_failure_counted(
                AuthFailure::ScramOther,
                bare_identifier.as_ref(),
            )];
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
pub(super) async fn handle_sasl_scram_response(
    b64_data: &str,
    domain: &str,
    state: &WebSocketState,
    mut scram: ScramPendingState,
    authenticated_session: &mut Option<Session>,
    phase: &mut ConnectionPhase,
) -> Vec<String> {
    let decoded = match BASE64_STANDARD.decode(b64_data.trim()) {
        Ok(data) => data,
        Err(_) => {
            let bare_identifier = scram_bare_identifier(scram.username(), domain);
            return vec![scram_failure_counted(
                AuthFailure::ScramMalformed,
                bare_identifier.as_ref(),
            )];
        }
    };

    let client_final = match String::from_utf8(decoded) {
        Ok(s) => s,
        Err(_) => {
            let bare_identifier = scram_bare_identifier(scram.username(), domain);
            return vec![scram_failure_counted(
                AuthFailure::ScramMalformed,
                bare_identifier.as_ref(),
            )];
        }
    };

    let server_final = match scram.process_client_final(&client_final) {
        Ok(result) => result,
        Err(_) => {
            let bare_identifier = scram_bare_identifier(scram.username(), domain);
            return vec![scram_failure_counted(
                AuthFailure::ScramInvalidCredentials,
                bare_identifier.as_ref(),
            )];
        }
    };

    // Authentication successful - create session
    let bare_jid_str = format!("{}@{}", scram.username(), domain);
    let full_jid = match format!("{}/pending", bare_jid_str).parse::<FullJid>() {
        Ok(jid) => jid,
        Err(_) => {
            return vec![scram_failure_counted(AuthFailure::ScramOther, None)];
        }
    };

    let bare_jid: BareJid = match bare_jid_str.parse() {
        Ok(jid) => jid,
        Err(_) => {
            return vec![scram_failure_counted(AuthFailure::ScramOther, None)];
        }
    };

    info!(
        jid = %bare_jid_str,
        "SASL SCRAM-SHA-256 authentication successful",
    );
    waddle_xmpp::metrics::record_auth_attempt("SCRAM-SHA-256", true);
    record_auth_success(AuthStage::Scram);

    let session = Session::new(&bare_jid.to_string(), scram.username(), scram.username());

    // Persist the session row: the XEP-0198 resume fence authorizes resume
    // exclusively against the durable principal, so an unpersisted SCRAM
    // session would detach with a principal ref that resolves Missing and
    // every resume would fail not-authorized. Authentication itself still
    // succeeds if the write fails — the session is just not resumable.
    if let Err(error) = state
        .deps
        .auth_state
        .session_manager
        .create_session(&session)
        .await
    {
        warn!(
            jid = %bare_jid_str,
            %error,
            "SCRAM session not persisted; SM resume will be unavailable for this session"
        );
    }

    *authenticated_session = Some(session);
    *phase = ConnectionPhase::authenticated(&full_jid);

    let success_b64 = BASE64_STANDARD.encode(server_final.message.as_bytes());
    vec![element_to_xml(
        Element::builder("success", waddle_xmpp::ns::SASL)
            .append(success_b64)
            .build(),
    )]
}
