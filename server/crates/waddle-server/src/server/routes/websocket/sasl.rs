use super::frame::{ResponseBatch, ResponseFrame};
use super::state::WsConnState;
use super::stream_management::InlineResumeStep;
use super::transport_xml::{sasl_failure_element, sasl_success_element};
use crate::auth::directory::exact_account_jid;
use crate::server::routes::auth_telemetry::{
    record_auth_failure, record_auth_success, AuthFailure,
};
use crate::sm_authority::{
    decide_bind2, inline_results, plan_inline, Bind2Decision, Bind2EnableOutcome,
    Bind2StreamManagement, DurableSmError, EnabledCommit, InlineOutcomeUnknown, InlinePlan,
    InlineResumeFailed, InlineResumeOutcome, InlineSession,
};
use waddle_xmpp::auth::OAuthBearerParseError;
use waddle_xmpp::identity::CanonicalBareJid;
use waddle_xmpp::protocol::OAuthSaslFailure;
use waddle_xmpp::telemetry::attributes::AuthStage;
use waddle_xmpp::xep::xep0493::{discovery_url, OAuthBearerError, OAuthBearerStatus};
use waddle_xmpp_core::xep0388_inline::{
    Bind2Request, Bind2Tag, Sasl2InlineRequests, Sasl2InlineResults, SmInlineEnable,
    SmInlineFailed, SmInlineResumeResult, UserAgentId,
};

/// End a failed OAUTHBEARER exchange the way RFC 7628 §3.2.2 requires: the
/// server answers with a challenge holding a JSON error document, the client
/// sends a dummy response, and only then does the mechanism fail. The
/// document names the RFC 8414 discovery document in `openid-configuration`
/// (XEP-0493), which is how a client that sent an empty token learns where
/// to authorize.
fn oauth_failure(
    base_url: &str,
    sasl2: bool,
    status: OAuthBearerStatus,
    failure: OAuthSaslFailure,
    authenticated_session: &mut Option<Session>,
    phase: &mut ConnectionPhase,
) -> Vec<Element> {
    waddle_xmpp::metrics::record_auth_attempt("OAUTHBEARER", false);
    *authenticated_session = None;
    let discovery_url = discovery_url(base_url);
    let json = OAuthBearerError::new(status, &discovery_url).to_json_bytes();
    if sasl2 {
        use waddle_xmpp_core::xep0388::{Sasl2Challenge, Sasl2Data};

        *phase = ConnectionPhase::sasl2_oauth_error_pending();
        vec![Sasl2Challenge {
            data: Sasl2Data::new(json),
        }
        .to_element()]
    } else {
        *phase = ConnectionPhase::sasl1_oauth_error_pending(failure);
        vec![Element::builder("challenge", waddle_xmpp::ns::SASL)
            .append(BASE64_STANDARD.encode(json))
            .build()]
    }
}

fn oauth_status_for_session_validation_error(error: &crate::auth::AuthError) -> OAuthBearerStatus {
    match error {
        crate::auth::AuthError::SessionNotFound | crate::auth::AuthError::SessionExpired => {
            OAuthBearerStatus::InvalidToken
        }
        _ => OAuthBearerStatus::ServerError,
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

pub(super) struct OAuthBearerContext<'a> {
    pub(super) domain: &'a str,
    pub(super) base_url: &'a str,
    pub(super) stream_from: Option<&'a BareJid>,
    pub(super) sasl2: bool,
}

/// Handle SASL OAUTHBEARER authentication.
pub(super) async fn handle_sasl_oauthbearer(
    b64_data: &str,
    ctx: OAuthBearerContext<'_>,
    state: &WebSocketState,
    authenticated_session: &mut Option<Session>,
    phase: &mut ConnectionPhase,
) -> Vec<Element> {
    let OAuthBearerContext {
        domain,
        base_url,
        stream_from,
        sasl2,
    } = ctx;
    // Every failure of the exchange ends as RFC 7628 §3.2.2 requires.
    macro_rules! fail {
        ($status:expr, $failure:expr) => {
            oauth_failure(
                base_url,
                sasl2,
                $status,
                $failure,
                authenticated_session,
                phase,
            )
        };
    }

    debug!("SASL OAUTHBEARER auth attempt");

    let decoded = match BASE64_STANDARD.decode(b64_data) {
        Ok(data) => data,
        Err(e) => {
            warn!(error = %e, "SASL OAUTHBEARER: failed to decode base64 data");
            return fail!(
                OAuthBearerStatus::InvalidRequest,
                OAuthSaslFailure::NotAuthorized
            );
        }
    };

    let credentials = match parse_oauthbearer(&decoded) {
        Ok(OAuthBearerResult::Credentials(credentials)) => credentials,
        Ok(OAuthBearerResult::DiscoveryRequest) => {
            debug!("SASL OAUTHBEARER: discovery request");
            return fail!(
                OAuthBearerStatus::InvalidToken,
                OAuthSaslFailure::NotAuthorized
            );
        }
        Err(OAuthBearerParseError::InvalidAuthzid) => {
            return fail!(
                OAuthBearerStatus::InvalidRequest,
                OAuthSaslFailure::InvalidAuthzid
            );
        }
        Err(e) => {
            warn!(error = %e, "SASL OAUTHBEARER: failed to parse bearer data");
            return fail!(
                OAuthBearerStatus::InvalidRequest,
                OAuthSaslFailure::NotAuthorized
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
            // The account is bound to its stored identity exactly: the
            // stored bare JID must be canonical, and the stored localpart at
            // this domain must name that same JID without any folding.
            let principal = match CanonicalBareJid::parse_exact(&session.user_jid) {
                Ok(principal) if principal.localpart().is_some() => principal.into_bare_jid(),
                _ => {
                    return fail!(
                        OAuthBearerStatus::ServerError,
                        OAuthSaslFailure::NotAuthorized
                    )
                }
            };
            if principal.domain().as_str() != domain {
                warn!(
                    session_domain = %principal.domain(),
                    connection_domain = %domain,
                    "SASL OAUTHBEARER: rejecting cross-tenant session token"
                );
                return fail!(
                    OAuthBearerStatus::InvalidToken,
                    OAuthSaslFailure::NotAuthorized
                );
            }
            let bare_jid_str = match localpart_to_jid(&session.xmpp_localpart, domain) {
                Ok(jid) => jid,
                Err(e) => {
                    warn!(
                        localpart = %session.xmpp_localpart,
                        error = %e,
                        "SASL OAUTHBEARER: failed to build JID from session localpart",
                    );
                    return fail!(
                        OAuthBearerStatus::ServerError,
                        OAuthSaslFailure::NotAuthorized
                    );
                }
            };

            let derived = match bare_jid_str.parse::<BareJid>() {
                Ok(derived) => derived,
                Err(_) => {
                    return fail!(
                        OAuthBearerStatus::ServerError,
                        OAuthSaslFailure::NotAuthorized
                    )
                }
            };
            if derived != principal {
                return fail!(
                    OAuthBearerStatus::ServerError,
                    OAuthSaslFailure::NotAuthorized
                );
            }
            if credentials
                .authzid
                .as_ref()
                .is_some_and(|authzid| authzid != &principal)
            {
                return fail!(
                    OAuthBearerStatus::InvalidRequest,
                    OAuthSaslFailure::InvalidAuthzid
                );
            }
            if stream_from.is_some_and(|from| from != &principal) {
                if !sasl2 {
                    // The stream's account hint disagrees with the token's
                    // account: the connection ends with a plain failure,
                    // not an error challenge that would leave it open.
                    waddle_xmpp::metrics::record_auth_attempt("OAUTHBEARER", false);
                    *authenticated_session = None;
                    *phase = ConnectionPhase::closing(None);
                    return vec![sasl_failure_element("not-authorized")];
                }
                return fail!(
                    OAuthBearerStatus::InvalidRequest,
                    OAuthSaslFailure::NotAuthorized
                );
            }

            let full_jid = match principal.with_resource_str("pending") {
                Ok(jid) => jid,
                Err(e) => {
                    warn!(jid = %bare_jid_str, error = %e, "SASL OAUTHBEARER: JID construction failed");
                    return fail!(
                        OAuthBearerStatus::ServerError,
                        OAuthSaslFailure::NotAuthorized
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
                return fail!(
                    OAuthBearerStatus::ServerError,
                    OAuthSaslFailure::NotAuthorized
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
            // Verifier errors stay out of logs and the wire response.
            warn!("SASL OAUTHBEARER session validation failed");
            fail!(status, OAuthSaslFailure::NotAuthorized)
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

    // Authentication successful - create session. The credentials were
    // found under exactly this username, so the bound JID is the account's
    // canonical stored identity, never a folded spelling of it.
    let Some(bare_jid) = exact_account_jid(scram.username(), domain).map(BareJid::from) else {
        return ScramResponse {
            element: scram_failure_counted(AuthFailure::ScramOther, None),
            server_final: None,
        };
    };
    let bare_jid_str = bare_jid.to_string();
    let full_jid: FullJid = match bare_jid.with_resource_str("pending") {
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

/// What a SASL2 `<authenticate/>` asks of the connection besides
/// authentication: its inline requests, and the client installation it names
/// in its `<user-agent/>`.
#[derive(Default)]
pub(super) struct Sasl2InlineExchange {
    requests: Sasl2InlineRequests,
    user_agent: Option<UserAgentId>,
}

/// What a Bind 2 request asks for besides a resource.
struct Bind2Plan {
    tag: Option<Bind2Tag>,
    /// A XEP-0280 `<enable/>` was requested: carbons are on from the start
    /// of the session.
    carbons: bool,
}

impl Bind2Plan {
    /// The plan for `request`, or `None` if it asks for a feature the server
    /// does not offer inline: Bind 2 advertises XEP-0280 and XEP-0198 only.
    fn of(request: &Bind2Request) -> Option<Self> {
        let mut carbons = false;
        for feature in &request.other_features {
            let is_carbons_enable = feature.is("enable", waddle_xmpp_core::carbons::CARBONS_NS)
                && feature.attrs().is_empty()
                && feature.nodes().next().is_none();
            if !is_carbons_enable || carbons {
                return None;
            }
            carbons = true;
        }
        Some(Self {
            tag: request.tag.clone(),
            carbons,
        })
    }
}

/// What the server offers inside the SASL2 feature of a connection, `None`
/// where SASL2 is not negotiated.
///
/// The inline extensions are offered where they are processed: where the
/// durable session authority is selected (see [`sasl2_inline_requests`]).
/// Bind 2 lists the session features it enables inline: Message Carbons and
/// stream management.
pub(super) fn sasl2_offer(state: &WebSocketState) -> Option<super::transport_xml::Sasl2Offer> {
    use waddle_xmpp_core::disco::Feature;
    use waddle_xmpp_core::xep0388_inline::{Bind2Feature, Sasl2InlineOffer, NS_SM};

    if !state.sasl2_negotiation_enabled() {
        return None;
    }
    let inline = super::stream_management::selected_store(state)
        .is_some()
        .then(|| Sasl2InlineOffer {
            stream_resumption: true,
            bind2: Some(Bind2Feature {
                inline_features: vec![
                    Feature::new(waddle_xmpp_core::carbons::CARBONS_NS),
                    Feature::new(NS_SM),
                ],
            }),
        });
    Some(super::transport_xml::Sasl2Offer { inline })
}

/// The XEP-0198 and XEP-0386 requests of a SASL2 `<authenticate/>`, if the
/// request is one this connection processes.
///
/// Inline requests are processed only where the durable session authority is
/// selected: an inline resume or enable is a durable operation. Elsewhere a
/// child other than `<user-agent/>` is refused, as is a malformed or repeated
/// inline request anywhere, and a Bind 2 request for a feature the server
/// does not offer inline. `None` is a malformed request.
pub(super) fn sasl2_inline_requests(
    state: &WebSocketState,
    request: &waddle_xmpp_core::xep0388::Sasl2Authenticate,
) -> Option<Sasl2InlineExchange> {
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
        return Some(Sasl2InlineExchange::default());
    }
    let requests = Sasl2InlineRequests::from_authenticate(request).ok()?;
    if let Some(bind) = &requests.bind {
        Bind2Plan::of(bind)?;
    }
    Some(Sasl2InlineExchange {
        requests,
        user_agent: UserAgentId::from_authenticate(request),
    })
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
    /// The client installation the exchange names.
    user_agent: Option<UserAgentId>,
    /// The XEP-0313 `<metadata/>` of the archive as the resource was bound,
    /// for `<bound/>`.
    archive_metadata: Option<Element>,
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
    exchange: Sasl2InlineExchange,
    additional_data: Option<Vec<u8>>,
    authorization_identifier: BareJid,
) -> ResponseBatch {
    let Sasl2InlineExchange {
        requests,
        user_agent,
    } = exchange;
    if requests.is_empty() {
        return vec![
            sasl2_success_element(additional_data, authorization_identifier.into(), Vec::new()),
            super::transport_xml::build_stream_features_element(true),
        ]
        .into();
    }
    *staged = Some(PendingSasl2Success {
        additional_data,
        authorization_identifier,
        requests,
        user_agent,
        archive_metadata: None,
        progress: InlineProgress::Requested,
    });
    ResponseBatch::default()
}

fn sasl2_success_element(
    additional_data: Option<Vec<u8>>,
    authorization_identifier: jid::Jid,
    inline_results: Vec<Element>,
) -> Element {
    waddle_xmpp_core::xep0388::Sasl2Success {
        additional_data: additional_data.map(waddle_xmpp_core::xep0388::Sasl2Data::new),
        authorization_identifier,
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
    let bind = match decide_bind2(plan_inline(&pending.requests), outcome.as_ref()) {
        Ok(Bind2Decision::Bind { request, enable }) => {
            // Requests for features the server does not offer were refused
            // when the exchange began.
            Bind2Plan::of(request).map(|plan| (plan, enable))
        }
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
    let bound = match &bind {
        Some((plan, _)) => bind2_resource(state, conn, plan).await,
        None => None,
    };
    pending.progress = match (bind, bound) {
        (Some((_, enable)), Some(archive_metadata)) => {
            pending.archive_metadata = archive_metadata;
            InlineProgress::Binding {
                enable,
                resume_failed,
            }
        }
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

/// The resource identifier Bind 2 generates (XEP-0386, "Resource identifier
/// generation"): `[client tag]/[server generated identifier]`, the tag
/// included as-is. A tag that is not a valid resource part, or none, leaves
/// the server's identifier alone.
fn bind2_full_jid(bare_jid: &BareJid, tag: Option<&Bind2Tag>) -> Option<FullJid> {
    let server_id = format!("ws-{}", uuid::Uuid::new_v4());
    tag.map(Bind2Tag::as_str)
        .filter(|tag| !tag.is_empty())
        .and_then(|tag| {
            bare_jid
                .with_resource_str(&format!("{tag}/{server_id}"))
                .ok()
        })
        .or_else(|| bare_jid.with_resource_str(&server_id).ok())
}

/// Bind a server-generated resource for Bind 2 and perform what XEP-0386
/// requires of the bind: enable the features asked for, clear the offline
/// messages the archive provides, and read the archive's state. Registration
/// publishes the resource in the same turn.
///
/// `None` if the connection cannot bind now; otherwise the `<metadata/>` of
/// the archive as of the bind, if it could be read.
async fn bind2_resource(
    state: &WebSocketState,
    conn: &mut WsConnState,
    plan: &Bind2Plan,
) -> Option<Option<Element>> {
    if conn.resume_endpoint_handoff_blocked || !conn.phase.allows_resource_binding() {
        return None;
    }
    let bare_jid = conn.phase.authenticated_bare_jid()?.clone();
    let full_jid = bind2_full_jid(&bare_jid, plan.tag.as_ref())?;
    info!(jid = %full_jid, "Resource bound by Bind 2");
    conn.phase = ConnectionPhase::ready(full_jid, false);
    // XEP-0386: "Message Carbons MUST be enabled at this point." Registration
    // publishes the flag with the resource, so the session starts with it.
    conn.carbons_enabled = plan.carbons;
    clear_offline_messages(state, &bare_jid).await;
    Some(archive_metadata(state, &bare_jid).await)
}

/// XEP-0386: "Clear the offline messages for this user, if any, without
/// sending them (as they will be provided by MAM)."
///
/// A stored row that refers to the archive is cleared: the client catches up
/// from the archive. A message stored whole is in no archive, so it stays and
/// is delivered by the XEP-0160 flush at initial presence. A row a flush has
/// already claimed is that flush's.
async fn clear_offline_messages(state: &WebSocketState, account: &BareJid) {
    let storage = &state.deps.protocol.pending_delivery_storage;
    let rows = match storage.list(account).await {
        Ok(rows) => rows,
        Err(error) => {
            warn!(%error, "reading the offline store to clear it for Bind 2 failed");
            return;
        }
    };
    for row in rows {
        if row.flushed_in_session.is_some() || !row.payload.is_archived() {
            continue;
        }
        if let Err(error) = storage.delete_unclaimed_row(&row.id).await {
            warn!(%error, "clearing an archived offline message for Bind 2 failed");
        }
    }
}

/// The XEP-0313 `<metadata/>` of the account's archive, which tells the
/// client what it must synchronise. XEP-0386: "The server SHOULD include a
/// <metadata/> element". `None` if the archive cannot be read.
async fn archive_metadata(state: &WebSocketState, account: &BareJid) -> Option<Element> {
    match state
        .deps
        .protocol
        .mam_storage
        .archive_bounds(account, waddle_xmpp::mam::MamArchiveKind::Personal, None)
        .await
    {
        Ok(bounds) => Some(waddle_xmpp_core::mam::build_metadata_element(
            bounds.as_ref(),
        )),
        Err(error) => {
            warn!(%error, "reading the archive state for Bind 2 failed");
            None
        }
    }
}

/// XEP-0386: "Upon successful binding of a resource, the server SHOULD
/// terminate any earlier sessions from the same client (identified by the
/// <user-agent> 'id' attribute in its SASL2 authentication request)."
///
/// Records the client on the new binding and ends the other live bindings of
/// the account that were made for it, as a replacing bind does.
fn end_earlier_client_sessions(
    state: &WebSocketState,
    conn: &WsConnState,
    user_agent: Option<UserAgentId>,
) {
    use waddle_xmpp::registry::{ForceDetachOrigin, ForceDetachRequest};

    let (Some(id), Some(jid), Some(owner)) = (
        user_agent,
        conn.phase.bound_jid(),
        conn.registry_owner.as_ref(),
    ) else {
        return;
    };
    let registry = &state.deps.protocol.connection_registry;
    let Some(entry) = registry.entry_if_owner(jid, owner) else {
        return;
    };
    entry.set_user_agent_id(Some(id));
    let account = jid.to_bare();
    for (earlier, entry) in registry.other_resources_of_user_agent(&account, id, jid) {
        let (ack, _ack) = tokio::sync::oneshot::channel();
        let offered = entry.try_force_detach_and_interrupt(ForceDetachRequest {
            origin: ForceDetachOrigin::FreshBindReplacement,
            target_binding: entry.binding_ref().cloned(),
            requester_bare_jid: account.clone(),
            ack,
        });
        if offered.is_err() {
            warn!(jid = %earlier, "an earlier session of the same client could not be ended");
        }
    }
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
                end_earlier_client_sessions(state, conn, pending.user_agent);
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
                        archive_metadata: pending.archive_metadata.as_ref(),
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
    // XEP-0386: after a bind the authorization identifier is the client's
    // full JID, which is how the client learns its generated resource.
    let authorization_identifier = match (&results.bound, conn.phase.bound_jid()) {
        (Some(_), Some(full)) => full.clone().into(),
        _ => pending.authorization_identifier.into(),
    };
    let success = sasl2_success_element(
        pending.additional_data,
        authorization_identifier,
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
                archive_metadata: self.archive_metadata.as_ref(),
            }),
            resumed: false,
        };
    }
}

#[cfg(test)]
mod tests {
    use super::{oauth_status_for_session_validation_error, OAuthBearerStatus};
    use crate::auth::AuthError;

    #[test]
    fn oauth_session_validation_errors_use_generic_oauth_statuses() {
        assert_eq!(
            oauth_status_for_session_validation_error(&AuthError::SessionNotFound),
            OAuthBearerStatus::InvalidToken
        );
        assert_eq!(
            oauth_status_for_session_validation_error(&AuthError::SessionExpired),
            OAuthBearerStatus::InvalidToken
        );
        assert_eq!(
            oauth_status_for_session_validation_error(&AuthError::DatabaseError(
                "db unavailable".to_owned()
            )),
            OAuthBearerStatus::ServerError
        );
    }
}
