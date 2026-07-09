use super::transport_xml::{
    element_to_xml, sasl_failure_xml, sasl_success_xml, SaslFailureCondition,
};
use super::*;
use waddle_xmpp::auth::{
    oauthbearer::{
        parse_oauthbearer, parse_oauthbearer_error_response, OAuthBearerErrorChallenge,
        OAuthBearerResult,
    },
    SaslInitialResponse, SaslResponsePayload, ScramFinalError,
};
use waddle_xmpp::prometheus::{
    increment_auth_terminal_attempt, AuthMechanism, AuthTerminalOutcome,
};

const fn scram_final_error_outcome(error: ScramFinalError) -> AuthTerminalOutcome {
    match error {
        ScramFinalError::InvalidCredentials => AuthTerminalOutcome::InvalidCredentials,
        ScramFinalError::Malformed => AuthTerminalOutcome::Malformed,
        ScramFinalError::InvalidState => AuthTerminalOutcome::InternalError,
    }
}

fn record_oauth_terminal(state: &WebSocketState, outcome: AuthTerminalOutcome) {
    state
        .deps
        .oauth_terminal_recorder
        .record(AuthMechanism::OAuthBearer, outcome);
}

/// Handle SASL OAUTHBEARER authentication.
pub(super) async fn handle_sasl_oauthbearer_initial(
    initial_response: &SaslInitialResponse,
    state: &WebSocketState,
    authenticated_session: &mut Option<Session>,
    phase: &mut ConnectionPhase,
) -> Vec<String> {
    if initial_response.is_empty() {
        *phase = ConnectionPhase::oauthbearer_initial_response_pending();
        return vec![element_to_xml(
            Element::builder("challenge", waddle_xmpp::ns::SASL).build(),
        )];
    }
    handle_sasl_oauthbearer_response_bytes(
        initial_response.as_bytes(),
        state,
        authenticated_session,
        phase,
    )
    .await
}

pub(super) async fn handle_sasl_oauthbearer_response(
    response: &SaslResponsePayload,
    state: &WebSocketState,
    authenticated_session: &mut Option<Session>,
    phase: &mut ConnectionPhase,
) -> Vec<String> {
    handle_sasl_oauthbearer_response_bytes(response.as_bytes(), state, authenticated_session, phase)
        .await
}

async fn handle_sasl_oauthbearer_response_bytes(
    response: &[u8],
    state: &WebSocketState,
    authenticated_session: &mut Option<Session>,
    phase: &mut ConnectionPhase,
) -> Vec<String> {
    debug!("SASL OAUTHBEARER auth attempt");

    let credentials = match parse_oauthbearer(response) {
        Ok(OAuthBearerResult::Credentials(credentials)) => credentials,
        Ok(OAuthBearerResult::EmptyCredentials) => return oauthbearer_invalid_token(state, phase),
        Err(_) => {
            warn!(
                category = "oauthbearer-malformed",
                "SASL OAUTHBEARER: failed to parse bearer data"
            );
            record_oauth_terminal(state, AuthTerminalOutcome::Malformed);
            return vec![sasl_failure_xml(SaslFailureCondition::MalformedRequest)];
        }
    };

    match state
        .deps
        .auth_state
        .session_manager
        .validate_session(credentials.token().expose_secret())
        .await
    {
        Ok(session) => {
            let bare_jid =
                match localpart_to_jid(&session.xmpp_localpart, &state.deps.auth_state.xmpp_domain)
                    .ok()
                    .and_then(|jid| jid.parse::<BareJid>().ok())
                {
                    Some(jid) => jid,
                    None => {
                        warn!("SASL OAUTHBEARER: failed to build JID from session localpart");
                        record_oauth_terminal(state, AuthTerminalOutcome::InternalError);
                        return vec![sasl_failure_xml(SaslFailureCondition::TemporaryAuthFailure)];
                    }
                };

            if credentials
                .authorization_identity()
                .is_some_and(|authzid| authzid != &bare_jid)
            {
                warn!("SASL OAUTHBEARER authorization identity does not match session");
                record_oauth_terminal(state, AuthTerminalOutcome::InvalidCredentials);
                return vec![sasl_failure_xml(SaslFailureCondition::InvalidAuthzid)];
            }

            let full_jid = match format!("{bare_jid}/pending").parse::<FullJid>() {
                Ok(jid) => jid,
                Err(_) => {
                    warn!("SASL OAUTHBEARER: JID construction failed");
                    record_oauth_terminal(state, AuthTerminalOutcome::InternalError);
                    return vec![sasl_failure_xml(SaslFailureCondition::TemporaryAuthFailure)];
                }
            };

            info!("SASL OAUTHBEARER authentication successful");

            *authenticated_session = Some(session);
            *phase = ConnectionPhase::authenticated(&full_jid);
            record_oauth_terminal(state, AuthTerminalOutcome::Success);

            vec![sasl_success_xml()]
        }
        Err(AuthError::SessionNotFound(_) | AuthError::SessionExpired) => {
            warn!("SASL OAUTHBEARER authentication failed");
            oauthbearer_invalid_token(state, phase)
        }
        Err(_) => {
            warn!("SASL OAUTHBEARER authentication failed internally");
            record_oauth_terminal(state, AuthTerminalOutcome::InternalError);
            vec![sasl_failure_xml(SaslFailureCondition::TemporaryAuthFailure)]
        }
    }
}

fn oauthbearer_invalid_token(state: &WebSocketState, phase: &mut ConnectionPhase) -> Vec<String> {
    let challenge = match OAuthBearerErrorChallenge::invalid_token().to_element() {
        Ok(challenge) => challenge,
        Err(_) => {
            warn!(
                category = "oauthbearer-challenge-encoding",
                "SASL OAUTHBEARER: failed to encode RFC 7628 error challenge"
            );
            record_oauth_terminal(state, AuthTerminalOutcome::InternalError);
            return vec![sasl_failure_xml(SaslFailureCondition::TemporaryAuthFailure)];
        }
    };
    *phase = ConnectionPhase::oauthbearer_error_pending();
    vec![element_to_xml(challenge)]
}

/// Complete the RFC 7628 failed-authentication sequence. The original
/// invalid-credential attempt becomes terminal only after this response.
pub(super) fn handle_sasl_oauthbearer_error_response(
    response: &SaslResponsePayload,
    state: &WebSocketState,
) -> Vec<String> {
    if parse_oauthbearer_error_response(response.as_bytes()).is_err() {
        warn!(
            category = "oauthbearer-error-response-malformed",
            "SASL OAUTHBEARER error response is not the RFC response octet"
        );
        record_oauth_terminal(state, AuthTerminalOutcome::Malformed);
        return vec![sasl_failure_xml(SaslFailureCondition::MalformedRequest)];
    }
    record_oauth_terminal(state, AuthTerminalOutcome::InvalidCredentials);
    vec![sasl_failure_xml(SaslFailureCondition::NotAuthorized)]
}

/// Handle SASL SCRAM-SHA-256 client-first-message.
///
/// Parses the client-first to extract the username, looks up stored SCRAM
/// credentials, creates a ScramServer with the user's salt/iterations, and
/// returns a `<challenge>` frame.
pub(super) async fn handle_sasl_scram_client_first(
    initial_response: &SaslInitialResponse,
    domain: &str,
    state: &WebSocketState,
    phase: &mut ConnectionPhase,
) -> Vec<String> {
    debug!("SASL SCRAM-SHA-256 auth attempt");

    let client_first = match String::from_utf8(initial_response.as_bytes().to_vec()) {
        Ok(s) => s,
        Err(_) => {
            warn!(
                category = "scram-client-first-invalid-utf8",
                "SCRAM: invalid UTF-8 in client-first"
            );
            increment_auth_terminal_attempt(
                AuthMechanism::ScramSha256,
                AuthTerminalOutcome::Malformed,
            );
            return vec![sasl_failure_xml(SaslFailureCondition::MalformedRequest)];
        }
    };

    // Parse username from client-first-message: "n,,n=<username>,r=<nonce>"
    // Use a temporary ScramServer to extract it.
    let username = {
        let mut tmp = ScramServer::new();
        match tmp.process_client_first(&client_first) {
            Ok(result) => result.username,
            Err(_) => {
                warn!(
                    category = "scram-client-first-malformed",
                    "SCRAM: failed to parse client-first"
                );
                increment_auth_terminal_attempt(
                    AuthMechanism::ScramSha256,
                    AuthTerminalOutcome::Malformed,
                );
                return vec![sasl_failure_xml(SaslFailureCondition::MalformedRequest)];
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
            warn!("SCRAM: user not found");
            increment_auth_terminal_attempt(
                AuthMechanism::ScramSha256,
                AuthTerminalOutcome::InvalidCredentials,
            );
            return vec![sasl_failure_xml(SaslFailureCondition::NotAuthorized)];
        }
        Err(_) => {
            warn!("SCRAM: credential lookup failed");
            increment_auth_terminal_attempt(
                AuthMechanism::ScramSha256,
                AuthTerminalOutcome::InternalError,
            );
            return vec![sasl_failure_xml(SaslFailureCondition::TemporaryAuthFailure)];
        }
    };

    // Create ScramServer with the user's stored salt and iterations, then
    // process the client-first again to produce a challenge with the correct params.
    let mut scram_server = ScramServer::with_salt_b64(creds.salt_b64, creds.iterations);
    let server_first = match scram_server.process_client_first(&client_first) {
        Ok(result) => result,
        Err(_) => {
            warn!(
                category = "scram-client-first-state",
                "SCRAM: failed to process client-first with stored params"
            );
            increment_auth_terminal_attempt(
                AuthMechanism::ScramSha256,
                AuthTerminalOutcome::InternalError,
            );
            return vec![sasl_failure_xml(SaslFailureCondition::TemporaryAuthFailure)];
        }
    };

    let challenge_b64 = BASE64_STANDARD.encode(server_first.message.as_bytes());
    debug!("SCRAM-SHA-256 challenge generated");

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
    response: &SaslResponsePayload,
    domain: &str,
    mut scram: ScramPendingState,
    authenticated_session: &mut Option<Session>,
    phase: &mut ConnectionPhase,
) -> Vec<String> {
    let client_final = match String::from_utf8(response.as_bytes().to_vec()) {
        Ok(s) => s,
        Err(_) => {
            warn!(
                category = "scram-client-final-invalid-utf8",
                "SCRAM: invalid UTF-8 in client-final"
            );
            increment_auth_terminal_attempt(
                AuthMechanism::ScramSha256,
                AuthTerminalOutcome::Malformed,
            );
            return vec![sasl_failure_xml(SaslFailureCondition::MalformedRequest)];
        }
    };

    let server_final = match scram.process_client_final(&client_final) {
        Ok(result) => result,
        Err(error) => {
            increment_auth_terminal_attempt(
                AuthMechanism::ScramSha256,
                scram_final_error_outcome(error),
            );
            let condition = match error {
                ScramFinalError::InvalidCredentials => {
                    warn!("SCRAM-SHA-256 authentication failed");
                    SaslFailureCondition::NotAuthorized
                }
                ScramFinalError::Malformed => {
                    warn!("SCRAM-SHA-256 client-final message was malformed");
                    SaslFailureCondition::MalformedRequest
                }
                ScramFinalError::InvalidState => {
                    warn!("SCRAM-SHA-256 client-final reached an invalid server state");
                    SaslFailureCondition::TemporaryAuthFailure
                }
            };
            return vec![sasl_failure_xml(condition)];
        }
    };

    // Authentication successful - create session
    let bare_jid_str = format!("{}@{}", scram.username(), domain);
    let full_jid = match format!("{}/pending", bare_jid_str).parse::<FullJid>() {
        Ok(jid) => jid,
        Err(_) => {
            warn!("SCRAM: JID construction failed");
            increment_auth_terminal_attempt(
                AuthMechanism::ScramSha256,
                AuthTerminalOutcome::InternalError,
            );
            return vec![sasl_failure_xml(SaslFailureCondition::TemporaryAuthFailure)];
        }
    };

    let bare_jid: BareJid = match bare_jid_str.parse() {
        Ok(jid) => jid,
        Err(_) => {
            warn!("SCRAM: bare JID parse failed");
            increment_auth_terminal_attempt(
                AuthMechanism::ScramSha256,
                AuthTerminalOutcome::InternalError,
            );
            return vec![sasl_failure_xml(SaslFailureCondition::TemporaryAuthFailure)];
        }
    };

    info!("SASL SCRAM-SHA-256 authentication successful");

    let session = Session::new(&bare_jid.to_string(), scram.username(), scram.username());

    *authenticated_session = Some(session);
    *phase = ConnectionPhase::authenticated(&full_jid);
    increment_auth_terminal_attempt(AuthMechanism::ScramSha256, AuthTerminalOutcome::Success);

    let success_b64 = BASE64_STANDARD.encode(server_final.message.as_bytes());
    vec![element_to_xml(
        Element::builder("success", waddle_xmpp::ns::SASL)
            .append(success_b64)
            .build(),
    )]
}

#[cfg(test)]
mod metric_tests {
    use super::*;

    #[test]
    fn scram_final_errors_map_to_exact_terminal_outcomes() {
        assert_eq!(
            scram_final_error_outcome(ScramFinalError::InvalidCredentials),
            AuthTerminalOutcome::InvalidCredentials
        );
        assert_eq!(
            scram_final_error_outcome(ScramFinalError::Malformed),
            AuthTerminalOutcome::Malformed
        );
        assert_eq!(
            scram_final_error_outcome(ScramFinalError::InvalidState),
            AuthTerminalOutcome::InternalError
        );
    }
}
