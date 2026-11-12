use super::frame::{ResponseBatch, ResponseFrame};
use super::state::WsConnState;
use super::stream_management::InlineResumeStep;
use super::transport_xml::{sasl_failure_element, sasl_success_element};
use crate::server::routes::auth_telemetry::{
    record_auth_failure, record_auth_success, AuthFailure,
};
use crate::sm_authority::{
    decide_bind2, inline_results, plan_inline, Bind2Decision, Bind2EnableOutcome,
    Bind2StreamManagement, DurableSmError, EnabledCommit, InlineOutcomeUnknown, InlinePlan,
    InlineResumeFailed, InlineResumeOutcome, InlineSession,
};
use waddle_xmpp::auth::OAuthBearerParseError;
use waddle_xmpp::telemetry::attributes::AuthStage;
use waddle_xmpp_core::xep0388_inline::{
    Sasl2InlineRequests, Sasl2InlineResults, SmInlineEnable, SmInlineFailed, SmInlineResumeResult,
};

fn oauth_failure(
    sasl2: bool,
    status: &'static str,
    sasl1_condition: &'static str,
    authenticated_session: &mut Option<Session>,
    phase: &mut ConnectionPhase,
) -> Vec<Element> {
    waddle_xmpp::metrics::record_auth_attempt("OAUTHBEARER", false);
    *authenticated_session = None;
    if sasl2 {
        use waddle_xmpp_core::xep0388::{Sasl2Challenge, Sasl2Data};

        *phase = ConnectionPhase::sasl2_oauth_error_pending();
        let json = format!("{{\"status\":\"{status}\"}}");
        vec![Sasl2Challenge {
            data: Sasl2Data::new(json.into_bytes()),
        }
        .to_element()]
    } else {
        vec![sasl_failure_element(sasl1_condition)]
    }
}

fn oauth_status_for_session_validation_error(error: &crate::auth::AuthError) -> &'static str {
    match error {
        crate::auth::AuthError::SessionNotFound(_) | crate::auth::AuthError::SessionExpired => {
            "invalid_token"
        }
        _ => "server_error",
    }
}

fn scram_bare_identifier(username: &str, domain: &str) -> Option<BareJid> {
    format!("{username}@{domain}").parse().ok()
}

pub(super) fn record_scram_failure(failure: AuthFailure<'_>, bare_identifier: Option<&BareJid>) {
    record_auth_failure("native", bare_identifier, failure);
    waddle_xmpp::metrics::record_auth_attempt("SCRAM-SHA-256", false);
}

fn scram_failure_counted(failure: AuthFailure<'_>, bare_identifier: Option<&BareJid>) -> Element {
    record_scram_failure(failure, bare_identifier);
    sasl_failure_element("not-authorized")
}
use super::{
    debug, info, localpart_to_jid, parse_oauthbearer, warn, BareJid, ConnectionPhase, Element,
    Engine, FullJid, NativeUserStore, OAuthBearerResult, ScramPendingState, ScramServer, Session,
    WebSocketState, BASE64_STANDARD,
};

/// Handle SASL OAUTHBEARER authentication.
pub(super) async fn handle_sasl_oauthbearer(
    b64_data: &str,
    state: &WebSocketState,
    stream_from: Option<&BareJid>,
    authenticated_session: &mut Option<Session>,
    phase: &mut ConnectionPhase,
    sasl2: bool,
) -> Vec<Element> {
    debug!("SASL OAUTHBEARER auth attempt");

    let decoded = match BASE64_STANDARD.decode(b64_data) {
        Ok(data) => data,
        Err(e) => {
            warn!(error = %e, "SASL OAUTHBEARER: failed to decode base64 data");
            return oauth_failure(
                sasl2,
                "invalid_request",
                "not-authorized",
                authenticated_session,
                phase,
            );
        }
    };

    let credentials = match parse_oauthbearer(&decoded) {
        Ok(OAuthBearerResult::Credentials(credentials)) => credentials,
        Ok(OAuthBearerResult::DiscoveryRequest) => {
            warn!("SASL OAUTHBEARER: discovery request received on token-auth WebSocket path");
            return oauth_failure(
                sasl2,
                "invalid_request",
                "not-authorized",
                authenticated_session,
                phase,
            );
        }
        Err(OAuthBearerParseError::InvalidAuthzid) => {
            return oauth_failure(
                sasl2,
                "invalid_request",
                "invalid-authzid",
                authenticated_session,
                phase,
            );
        }
        Err(e) => {
            warn!(error = %e, "SASL OAUTHBEARER: failed to parse bearer data");
            return oauth_failure(
                sasl2,
                "invalid_request",
                "not-authorized",
                authenticated_session,
                phase,
            );
        }
    };

    match state
        .deps
        .auth_state
        .session_manager
        .validate_session_without_touch(&credentials.token)
        .await
    {
        Ok(session) => {
            let principal = match session.user_jid.parse::<BareJid>() {
                Ok(principal) if principal.node().is_some() => principal,
                _ => {
                    return oauth_failure(
                        sasl2,
                        "server_error",
                        "not-authorized",
                        authenticated_session,
                        phase,
                    )
                }
            };
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
                        return oauth_failure(
                            sasl2,
                            "server_error",
                            "not-authorized",
                            authenticated_session,
                            phase,
                        );
                    }
                };

            let derived = match bare_jid_str.parse::<BareJid>() {
                Ok(derived) => derived,
                Err(_) => {
                    return oauth_failure(
                        sasl2,
                        "server_error",
                        "not-authorized",
                        authenticated_session,
                        phase,
                    )
                }
            };
            if derived != principal {
                return oauth_failure(
                    sasl2,
                    "server_error",
                    "not-authorized",
                    authenticated_session,
                    phase,
                );
            }
            if credentials
                .authzid
                .as_ref()
                .is_some_and(|authzid| authzid != &principal)
            {
                return oauth_failure(
                    sasl2,
                    "invalid_request",
                    "invalid-authzid",
                    authenticated_session,
                    phase,
                );
            }
            if stream_from.is_some_and(|from| from != &principal) {
                if !sasl2 {
                    *phase = ConnectionPhase::closing(None);
                }
                return oauth_failure(
                    sasl2,
                    "invalid_request",
                    "not-authorized",
                    authenticated_session,
                    phase,
                );
            }

            let full_jid = match principal.with_resource_str("pending") {
                Ok(jid) => jid,
                Err(e) => {
                    warn!(jid = %bare_jid_str, error = %e, "SASL OAUTHBEARER: JID construction failed");
                    return oauth_failure(
                        sasl2,
                        "server_error",
                        "not-authorized",
                        authenticated_session,
                        phase,
                    );
                }
            };

            if state
                .deps
                .auth_state
                .session_manager
                .touch_session(&credentials.token)
                .await
                .is_err()
            {
                return oauth_failure(
                    sasl2,
                    "server_error",
                    "not-authorized",
                    authenticated_session,
                    phase,
                );
            }

            info!(
                jid = %bare_jid_str,
                user_jid = %session.user_jid,
                "SASL OAUTHBEARER authentication successful",
            );

            *authenticated_session = Some(session);
            *phase = ConnectionPhase::authenticated(&full_jid);

            waddle_xmpp::metrics::record_auth_attempt("OAUTHBEARER", true);
            vec![sasl_success_element()]
        }
        Err(e) => {
            let status = oauth_status_for_session_validation_error(&e);
            // SessionNotFound formats the presented bearer token, so do not
            // include verifier errors in logs or the wire response.
            warn!("SASL OAUTHBEARER session validation failed");
            oauth_failure(
                sasl2,
                status,
                "not-authorized",
                authenticated_session,
                phase,
            )
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
    sasl2: bool,
) -> Vec<Element> {
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

    let challenge = if sasl2 {
        waddle_xmpp_core::xep0388::Sasl2Challenge {
            data: waddle_xmpp_core::xep0388::Sasl2Data::new(server_first.message.into_bytes()),
        }
        .to_element()
    } else {
        let challenge_b64 = BASE64_STANDARD.encode(server_first.message.as_bytes());
        Element::builder("challenge", waddle_xmpp::ns::SASL)
            .append(challenge_b64)
            .build()
    };
    debug!(username = %username, "SCRAM-SHA-256 challenge generated");

    let pending = if sasl2 {
        ScramPendingState::new_sasl2(scram_server, creds.stored_key, creds.server_key, username)
    } else {
        ScramPendingState::new(scram_server, creds.stored_key, creds.server_key, username)
    };
    *phase = ConnectionPhase::scram_pending(pending);

    vec![challenge]
}

/// Handle SASL SCRAM-SHA-256 response (client-final-message).
///
/// Verifies the client proof against stored keys and returns `<success>` or
/// `<failure>`.
pub(super) struct ScramResponse {
    pub(super) element: Element,
    pub(super) server_final: Option<Vec<u8>>,
}

pub(super) async fn handle_sasl_scram_response(
    b64_data: &str,
    domain: &str,
    state: &WebSocketState,
    mut scram: ScramPendingState,
    stream_from: Option<&BareJid>,
    authenticated_session: &mut Option<Session>,
    phase: &mut ConnectionPhase,
) -> ScramResponse {
    let decoded = match BASE64_STANDARD.decode(b64_data.trim()) {
        Ok(data) => data,
        Err(_) => {
            let bare_identifier = scram_bare_identifier(scram.username(), domain);
            return ScramResponse {
                element: scram_failure_counted(
                    AuthFailure::ScramMalformed,
                    bare_identifier.as_ref(),
                ),
                server_final: None,
            };
        }
    };

    let client_final = match String::from_utf8(decoded) {
        Ok(s) => s,
        Err(_) => {
            let bare_identifier = scram_bare_identifier(scram.username(), domain);
            return ScramResponse {
                element: scram_failure_counted(
                    AuthFailure::ScramMalformed,
                    bare_identifier.as_ref(),
                ),
                server_final: None,
            };
        }
    };

    let server_final = match scram.process_client_final(&client_final) {
        Ok(result) => result,
        Err(_) => {
            let bare_identifier = scram_bare_identifier(scram.username(), domain);
            return ScramResponse {
                element: scram_failure_counted(
                    AuthFailure::ScramInvalidCredentials,
                    bare_identifier.as_ref(),
                ),
                server_final: None,
            };
        }
    };

    // Authentication successful - create session
    let bare_jid_str = format!("{}@{}", scram.username(), domain);
    let full_jid = match format!("{}/pending", bare_jid_str).parse::<FullJid>() {
        Ok(jid) => jid,
        Err(_) => {
            return ScramResponse {
                element: scram_failure_counted(AuthFailure::ScramOther, None),
                server_final: None,
            };
        }
    };

    let bare_jid: BareJid = match bare_jid_str.parse() {
        Ok(jid) => jid,
        Err(_) => {
            return ScramResponse {
                element: scram_failure_counted(AuthFailure::ScramOther, None),
                server_final: None,
            };
        }
    };

    if stream_from.is_some_and(|from| from != &bare_jid) {
        *phase = ConnectionPhase::closing(None);
        return ScramResponse {
            element: scram_failure_counted(AuthFailure::ScramOther, None),
            server_final: None,
        };
    }

    info!(
        jid = %bare_jid_str,
        "SASL SCRAM-SHA-256 authentication successful",
    );
    waddle_xmpp::metrics::record_auth_attempt("SCRAM-SHA-256", true);
    record_auth_success(AuthStage::Scram);

    // Native-resume row: backs the durable SM resume fence only. Its id is
    // never issued to the client and the row is rejected by every bearer
    // path (OAUTHBEARER, HTTP session auth) via the non-bearer sentinel.
    let session =
        Session::new_native_resume(&bare_jid.to_string(), scram.username(), scram.username());

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

    let server_final = server_final.message.into_bytes();
    let success_b64 = BASE64_STANDARD.encode(&server_final);
    ScramResponse {
        element: Element::builder("success", waddle_xmpp::ns::SASL)
            .append(success_b64)
            .build(),
        server_final: Some(server_final),
    }
}

/// The XEP-0198 and XEP-0386 requests of a SASL2 `<authenticate/>`, if the
/// request is one this connection processes.
///
/// Inline requests are processed only where the durable session authority is
/// selected: an inline resume or enable is a durable operation. Elsewhere a
/// child other than `<user-agent/>` is refused, as is a malformed or repeated
/// inline request anywhere. `None` is a malformed request.
pub(super) fn sasl2_inline_requests(
    state: &WebSocketState,
    request: &waddle_xmpp_core::xep0388::Sasl2Authenticate,
) -> Option<Sasl2InlineRequests> {
    use waddle_xmpp_core::xep0388::NS_SASL2;
    use waddle_xmpp_core::xep0388_inline::{NS_BIND2, NS_SM};

    let durable = super::stream_management::selected_store(state).is_some();
    let processed = |child: &Element| {
        child.is("user-agent", NS_SASL2)
            || (durable && (child.is("resume", NS_SM) || child.is("bind", NS_BIND2)))
    };
    if !request.additional_children().iter().all(processed) {
        return None;
    }
    if !durable {
        return Some(Sasl2InlineRequests::default());
    }
    Sasl2InlineRequests::from_authenticate(request).ok()
}

/// A SASL2 `<success/>` whose inline results are not all known yet.
///
/// Authentication has succeeded. The reply waits for the inline requests:
/// a resume is reported once its endpoint is published and the resume is
/// completed, and a Bind 2 `<bound/>` once the resource is registered and
/// its stream-management session, if requested, has committed.
pub(super) struct PendingSasl2Success {
    additional_data: Option<Vec<u8>>,
    authorization_identifier: BareJid,
    requests: Sasl2InlineRequests,
    progress: InlineProgress,
}

enum InlineProgress {
    /// The requests have not been processed.
    Requested,
    /// The resume committed; publication decides what is reported.
    Resuming,
    /// The resume's commit outcome is unknown. Nothing is reported and no
    /// bind is made until it is settled.
    ResumeHeld,
    /// A held resume was settled as a known failure; the bind decision
    /// follows.
    ResumeFailed(InlineResumeFailed),
    /// A resource is bound for Bind 2 and awaits registration.
    Binding {
        enable: Option<SmInlineEnable>,
        resume_failed: Option<InlineResumeFailed>,
    },
    /// The enable's commit outcome is unknown; `<bound/>` waits for it.
    EnableHeld {
        resumable: bool,
        resume_failed: Option<InlineResumeFailed>,
    },
    /// Every result is known.
    Ready {
        results: Sasl2InlineResults,
        /// A session resumed: the reply opens its replay batch, and no
        /// stream features follow.
        resumed: bool,
    },
}

/// The `<success/>` of a SASL2 exchange and what follows it.
///
/// Without inline requests that is the reply and the post-authentication
/// stream features, at once. With them nothing is answered here: the reply
/// is staged on the connection and written when its inline results are
/// known.
pub(super) fn sasl2_success_responses(
    staged: &mut Option<PendingSasl2Success>,
    requests: Sasl2InlineRequests,
    additional_data: Option<Vec<u8>>,
    authorization_identifier: BareJid,
) -> ResponseBatch {
    if requests.is_empty() {
        return vec![
            sasl2_success_element(additional_data, authorization_identifier, Vec::new()),
            super::transport_xml::build_stream_features_element(true),
        ]
        .into();
    }
    *staged = Some(PendingSasl2Success {
        additional_data,
        authorization_identifier,
        requests,
        progress: InlineProgress::Requested,
    });
    ResponseBatch::default()
}

fn sasl2_success_element(
    additional_data: Option<Vec<u8>>,
    authorization_identifier: BareJid,
    inline_results: Vec<Element>,
) -> Element {
    waddle_xmpp_core::xep0388::Sasl2Success {
        additional_data: additional_data.map(waddle_xmpp_core::xep0388::Sasl2Data::new),
        authorization_identifier: authorization_identifier.into(),
        inline_results,
    }
    .to_element()
}

/// The inline result of a resume that cannot be reported as resumed for a
/// reason the authority did not decide.
fn resume_not_published() -> Sasl2InlineResults {
    Sasl2InlineResults {
        resume: Some(SmInlineResumeResult::Failed(SmInlineFailed {
            h: None,
            condition: Some(xmpp_parsers::stanza_error::DefinedCondition::InternalServerError),
        })),
        ..Sasl2InlineResults::default()
    }
}

/// Build inline results, reporting nothing inline if they cannot be built.
fn results_or_none(session: InlineSession<'_>) -> Sasl2InlineResults {
    inline_results(session).unwrap_or_else(|error| {
        warn!(%error, "SASL2 inline results could not be built");
        Sasl2InlineResults::default()
    })
}

/// Process a staged exchange's inline requests up to the point registration
/// can act on them: run the resume, and bind a resource if Bind 2 applies.
///
/// A committed resume leaves the connection staged for publication and
/// skips the bind. A known resume failure, or no resume request, lets the
/// bind proceed. An unknown resume outcome holds the exchange: nothing is
/// bound and nothing is answered.
pub(super) async fn advance_sasl2_inline(state: &WebSocketState, conn: &mut WsConnState) {
    let Some(mut pending) = conn.sasl2_success.take() else {
        return;
    };
    let resume_failed = match std::mem::replace(&mut pending.progress, InlineProgress::Requested) {
        InlineProgress::Requested => match plan_inline(&pending.requests) {
            InlinePlan::Resume { resume, .. } => {
                match super::stream_management::inline_resume(state, conn, resume).await {
                    InlineResumeStep::Resumed => {
                        pending.progress = InlineProgress::Resuming;
                        conn.sasl2_success = Some(pending);
                        return;
                    }
                    InlineResumeStep::Held => {
                        pending.progress = InlineProgress::ResumeHeld;
                        conn.sasl2_success = Some(pending);
                        return;
                    }
                    InlineResumeStep::Failed(failed) => Some(failed),
                }
            }
            InlinePlan::Bind(_) | InlinePlan::None => None,
        },
        InlineProgress::ResumeFailed(failed) => Some(failed),
        other => {
            pending.progress = other;
            conn.sasl2_success = Some(pending);
            return;
        }
    };
    let outcome = resume_failed.map(InlineResumeOutcome::Failed);
    let enable = match decide_bind2(plan_inline(&pending.requests), outcome.as_ref()) {
        Ok(Bind2Decision::Bind { enable, .. }) => Some(enable),
        Ok(Bind2Decision::NotRequested | Bind2Decision::SkippedForResume) => None,
        Err(InlineOutcomeUnknown) => {
            pending.progress = InlineProgress::ResumeHeld;
            conn.sasl2_success = Some(pending);
            return;
        }
    };
    let resume_failed = match outcome {
        Some(InlineResumeOutcome::Failed(failed)) => Some(failed),
        _ => None,
    };
    pending.progress = match enable {
        Some(enable) if bind2_resource(conn) => InlineProgress::Binding {
            enable,
            resume_failed,
        },
        // No bind was requested, or the connection cannot bind now: the
        // client is told of the resume failure, if any, and binds itself.
        _ => InlineProgress::Ready {
            results: match &resume_failed {
                Some(failed) => results_or_none(InlineSession::ResumeFailed(failed)),
                None => Sasl2InlineResults::default(),
            },
            resumed: false,
        },
    };
    conn.sasl2_success = Some(pending);
}

/// Bind a server-generated resource for Bind 2. Registration publishes it
/// in the same turn.
fn bind2_resource(conn: &mut WsConnState) -> bool {
    if conn.resume_endpoint_handoff_blocked || !conn.phase.allows_resource_binding() {
        return false;
    }
    let Some(bare_jid) = conn.phase.authenticated_bare_jid() else {
        return false;
    };
    let Ok(full_jid) = bare_jid.with_resource_str(&format!("ws-{}", uuid::Uuid::new_v4())) else {
        return false;
    };
    info!(jid = %full_jid, "Resource bound by Bind 2");
    conn.phase = ConnectionPhase::ready(full_jid, false);
    true
}

/// Write a staged `<success/>` into `responses` once its inline results are
/// known, after registration ran for the turn.
///
/// A resource bound for Bind 2 gets its stream-management session here, on
/// the binding registration just published. A resumed session's reply takes
/// the place of the `<resumed/>` nonza at the head of its replay batch and
/// no stream features follow; every other reply is followed by the
/// post-authentication stream features. An exchange that is still held
/// writes nothing.
pub(super) async fn finish_sasl2_inline(
    state: &WebSocketState,
    conn: &mut WsConnState,
    responses: &mut ResponseBatch,
) {
    let Some(mut pending) = conn.sasl2_success.take() else {
        return;
    };
    // Whether the reply announces a session enabled on this binding. Such a
    // reply is written under the binding's permit, as `<enabled/>` is, so a
    // binding replaced meanwhile never announces it.
    let mut announces_session = false;
    let (results, resumed) = match pending.progress {
        InlineProgress::Ready { results, resumed } => {
            announces_session = !resumed && conn.pending_sm_enable_commit.is_some();
            (results, resumed)
        }
        // The resume was staged and this turn did not publish it.
        InlineProgress::Resuming => (resume_not_published(), false),
        InlineProgress::Binding {
            enable,
            resume_failed,
        } => {
            if conn.registry_owner.is_none() {
                // The resource was not registered, so no `<bound/>` is due.
                let results = match &resume_failed {
                    Some(failed) => results_or_none(InlineSession::ResumeFailed(failed)),
                    None => Sasl2InlineResults::default(),
                };
                (results, false)
            } else {
                let outcome = match enable {
                    Some(enable) => Some((
                        enable.resume,
                        super::stream_management::inline_enable(state, conn, enable).await,
                    )),
                    None => None,
                };
                if let Some((resumable, Bind2EnableOutcome::Unknown(_))) = outcome {
                    pending.progress = InlineProgress::EnableHeld {
                        resumable,
                        resume_failed,
                    };
                    conn.sasl2_success = Some(pending);
                    return;
                }
                let stream_management = match &outcome {
                    Some((resumable, Bind2EnableOutcome::Enabled(commit))) => {
                        announces_session = true;
                        Some(Bind2StreamManagement::Enabled {
                            commit,
                            resumable: *resumable,
                        })
                    }
                    Some((_, Bind2EnableOutcome::Failed(failed))) => {
                        Some(Bind2StreamManagement::Failed(failed))
                    }
                    Some((_, Bind2EnableOutcome::Unknown(_))) | None => None,
                };
                (
                    results_or_none(InlineSession::Bound {
                        resume_failed: resume_failed.as_ref(),
                        stream_management,
                    }),
                    false,
                )
            }
        }
        held @ (InlineProgress::Requested
        | InlineProgress::ResumeHeld
        | InlineProgress::ResumeFailed(_)
        | InlineProgress::EnableHeld { .. }) => {
            pending.progress = held;
            conn.sasl2_success = Some(pending);
            return;
        }
    };
    let success = sasl2_success_element(
        pending.additional_data,
        pending.authorization_identifier,
        results.to_elements(),
    );
    if resumed {
        match responses.frames.first_mut() {
            Some(first) => *first = success.into(),
            None => responses.frames.push(success.into()),
        }
    } else {
        *responses = ResponseBatch::from_frames(vec![
            if announces_session {
                ResponseFrame::SmEnabled(success)
            } else {
                ResponseFrame::from(success)
            },
            ResponseFrame::from(super::transport_xml::build_stream_features_element(true)),
        ]);
    }
}

impl PendingSasl2Success {
    /// The resume completed: `results` report it, or nothing if they could
    /// not be built.
    pub(super) fn resumed(&mut self, results: Result<Sasl2InlineResults, DurableSmError>) {
        self.progress = InlineProgress::Ready {
            results: results.unwrap_or_else(|error| {
                warn!(%error, "SASL2 inline resume result could not be built");
                Sasl2InlineResults::default()
            }),
            resumed: true,
        };
    }

    /// The committed resume was not published and has been returned to the
    /// authority.
    pub(super) fn resume_not_published(&mut self) {
        if matches!(
            self.progress,
            InlineProgress::Resuming | InlineProgress::ResumeHeld
        ) {
            self.progress = InlineProgress::Ready {
                results: resume_not_published(),
                resumed: false,
            };
        }
    }

    /// A held resume was settled as committed and staged for publication.
    pub(super) fn held_resume_committed(&mut self) {
        if matches!(self.progress, InlineProgress::ResumeHeld) {
            self.progress = InlineProgress::Resuming;
        }
    }

    /// A held resume was settled as a known failure.
    pub(super) fn held_resume_failed(&mut self, failed: InlineResumeFailed) {
        if matches!(self.progress, InlineProgress::ResumeHeld) {
            self.progress = InlineProgress::ResumeFailed(failed);
        }
    }

    /// A held enable was settled: `commit` if it committed.
    pub(super) fn held_enable_settled(&mut self, commit: Option<&EnabledCommit>) {
        let InlineProgress::EnableHeld {
            resumable,
            resume_failed,
        } = std::mem::replace(&mut self.progress, InlineProgress::Requested)
        else {
            return;
        };
        let failed = SmInlineFailed {
            h: None,
            condition: Some(xmpp_parsers::stanza_error::DefinedCondition::InternalServerError),
        };
        self.progress = InlineProgress::Ready {
            results: results_or_none(InlineSession::Bound {
                resume_failed: resume_failed.as_ref(),
                stream_management: Some(match commit {
                    Some(commit) => Bind2StreamManagement::Enabled { commit, resumable },
                    None => Bind2StreamManagement::Failed(&failed),
                }),
            }),
            resumed: false,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::oauth_status_for_session_validation_error;
    use crate::auth::AuthError;

    #[test]
    fn oauth_session_validation_errors_use_generic_oauth_statuses() {
        assert_eq!(
            oauth_status_for_session_validation_error(&AuthError::SessionNotFound(
                "secret-token".to_owned()
            )),
            "invalid_token"
        );
        assert_eq!(
            oauth_status_for_session_validation_error(&AuthError::SessionExpired),
            "invalid_token"
        );
        assert_eq!(
            oauth_status_for_session_validation_error(&AuthError::DatabaseError(
                "db unavailable".to_owned()
            )),
            "server_error"
        );
    }
}
