use super::*;

fn oauthbearer_auth_frame(decoded: &[u8]) -> String {
    element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(BASE64_STANDARD.encode(decoded))
            .build(),
    )
}

fn sasl_response_frame(decoded: &[u8]) -> String {
    element_to_xml(
        Element::builder("response", waddle_xmpp::ns::SASL)
            .append(BASE64_STANDARD.encode(decoded))
            .build(),
    )
}

type AuthRecords = Arc<
    std::sync::Mutex<
        Vec<(
            waddle_xmpp::prometheus::AuthMechanism,
            waddle_xmpp::prometheus::AuthTerminalOutcome,
        )>,
    >,
>;

async fn create_recording_oauth_test_state() -> (Arc<WebSocketState>, AuthRecords) {
    let records = Arc::new(std::sync::Mutex::new(Vec::new()));
    let sink = Arc::clone(&records);
    let mut state = create_test_websocket_state().await;
    Arc::get_mut(&mut state)
        .expect("unshared test state")
        .deps
        .oauth_terminal_recorder = OAuthTerminalRecorder::new(move |mechanism, outcome| {
        sink.lock()
            .expect("auth records mutex")
            .push((mechanism, outcome));
    });
    (state, records)
}

#[tokio::test]
async fn oauthbearer_empty_credential_completes_rfc7628_failed_exchange_once() {
    let (state, records) = create_recording_oauth_test_state().await;
    let mut conn = WsConnState::new();
    let auth = oauthbearer_auth_frame(b"n,,\x01auth=\x01\x01");

    let challenge_responses =
        handle_xmpp_frame(&auth, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(challenge_responses.len(), 1);
    let challenge = Element::from_str(&challenge_responses[0]).expect("challenge XML");
    assert_eq!(challenge.name(), "challenge");
    assert_eq!(challenge.ns(), waddle_xmpp::ns::SASL);
    let challenge_json = BASE64_STANDARD
        .decode(challenge.text())
        .expect("base64 challenge");
    let challenge_json: serde_json::Value =
        serde_json::from_slice(&challenge_json).expect("JSON challenge");
    assert_eq!(
        challenge_json,
        serde_json::json!({"status": "invalid_token"})
    );
    assert!(conn.phase.has_pending_oauthbearer_exchange());
    assert!(conn.authenticated_session.is_none());
    assert!(records.lock().expect("auth records").is_empty());

    let abort = sasl_response_frame(b"\x01");
    let terminal = handle_xmpp_frame(&abort, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(
        terminal,
        vec![sasl_failure_xml(SaslFailureCondition::NotAuthorized)]
    );
    assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
    assert!(conn.authenticated_session.is_none());
    assert_eq!(
        *records.lock().expect("auth records"),
        vec![(
            waddle_xmpp::prometheus::AuthMechanism::OAuthBearer,
            waddle_xmpp::prometheus::AuthTerminalOutcome::InvalidCredentials,
        )]
    );
}

#[tokio::test]
async fn oauthbearer_unknown_session_token_uses_the_same_failed_exchange() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let auth = oauthbearer_auth_frame(b"n,,\x01auth=Bearer absent-session-token\x01\x01");

    let challenge = handle_xmpp_frame(&auth, "example.com", state.as_ref(), &mut conn).await;
    let challenge = Element::from_str(&challenge[0]).expect("challenge XML");
    let decoded = BASE64_STANDARD
        .decode(challenge.text())
        .expect("base64 challenge");
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&decoded).expect("JSON challenge"),
        serde_json::json!({"status": "invalid_token"})
    );
    assert!(conn.phase.has_pending_oauthbearer_exchange());

    let terminal = sasl_response_frame(b"\x01");
    assert_eq!(
        handle_xmpp_frame(&terminal, "example.com", state.as_ref(), &mut conn).await,
        vec![sasl_failure_xml(SaslFailureCondition::NotAuthorized)]
    );
}

#[tokio::test]
async fn oauthbearer_expired_session_token_uses_the_failed_exchange() {
    let state = create_test_websocket_state().await;
    let mut session = Session::new("alice", "alice", "alice");
    session.expires_at = Some(chrono::Utc::now() - chrono::Duration::seconds(1));
    state
        .deps
        .auth_state
        .session_manager
        .create_session(&session)
        .await
        .expect("expired session fixture");
    let auth =
        oauthbearer_auth_frame(format!("n,,\x01auth=Bearer {}\x01\x01", session.id).as_bytes());
    let mut conn = WsConnState::new();

    let challenge = handle_xmpp_frame(&auth, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(
        Element::from_str(&challenge[0]).expect("challenge").name(),
        "challenge"
    );
    assert!(conn.phase.has_pending_oauthbearer_exchange());
    let terminal = sasl_response_frame(b"\x01");
    assert_eq!(
        handle_xmpp_frame(&terminal, "example.com", state.as_ref(), &mut conn).await,
        vec![sasl_failure_xml(SaslFailureCondition::NotAuthorized)]
    );
}

#[tokio::test]
async fn oauthbearer_error_exchange_accepts_only_rfc7628_response_octet() {
    let state = create_test_websocket_state().await;

    for (response, condition) in [
        (
            r#"<response xmlns="urn:ietf:params:xml:ns:xmpp-sasl">%%%not-base64</response>"#
                .to_string(),
            SaslFailureCondition::IncorrectEncoding,
        ),
        (
            sasl_response_frame(b"not-an-abort"),
            SaslFailureCondition::MalformedRequest,
        ),
    ] {
        let mut conn = WsConnState::new();
        let auth = oauthbearer_auth_frame(b"n,,\x01auth=Bearer \x01\x01");
        let challenge = handle_xmpp_frame(&auth, "example.com", state.as_ref(), &mut conn).await;
        assert_eq!(challenge.len(), 1);
        assert!(conn.phase.has_pending_oauthbearer_exchange());

        let terminal = handle_xmpp_frame(&response, "example.com", state.as_ref(), &mut conn).await;
        assert_eq!(terminal, vec![sasl_failure_xml(condition)]);
        assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
    }
}

#[tokio::test]
async fn oauthbearer_error_exchange_accepts_typed_sasl_abort() {
    let (state, records) = create_recording_oauth_test_state().await;
    let auth = oauthbearer_auth_frame(b"n,,\x01auth=\x01\x01");
    let abort = element_to_xml(Element::builder("abort", waddle_xmpp::ns::SASL).build());
    let mut conn = WsConnState::new();

    let challenge = handle_xmpp_frame(&auth, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(challenge.len(), 1);
    assert!(conn.phase.has_pending_oauthbearer_exchange());

    let terminal = handle_xmpp_frame(&abort, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(
        terminal,
        vec![sasl_failure_xml(SaslFailureCondition::Aborted)]
    );
    assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
    assert_eq!(
        *records.lock().expect("auth records"),
        vec![(
            waddle_xmpp::prometheus::AuthMechanism::OAuthBearer,
            waddle_xmpp::prometheus::AuthTerminalOutcome::Cancelled,
        )]
    );
}

#[tokio::test]
async fn oauthbearer_without_initial_response_uses_rfc6120_challenge_mode() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let auth = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .build(),
    );
    let mut conn = WsConnState::new();

    let challenge = handle_xmpp_frame(&auth, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(challenge.len(), 1);
    let challenge = Element::from_str(&challenge[0]).expect("empty challenge XML");
    assert_eq!(challenge.name(), "challenge");
    assert!(challenge.text().is_empty());
    assert!(conn.phase.has_pending_oauthbearer_exchange());

    let response =
        sasl_response_frame(format!("n,,\x01auth=Bearer {}\x01\x01", session.id).as_bytes());
    let terminal = handle_xmpp_frame(&response, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(terminal, vec![sasl_success_xml()]);
    assert!(conn.phase.is_authenticated());
}

#[tokio::test]
async fn oauthbearer_enforces_matching_bare_authorization_identity() {
    let (state, records) = create_recording_oauth_test_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let mismatched = oauthbearer_auth_frame(
        format!(
            "n,a=mallory@example.com,\x01auth=Bearer {}\x01\x01",
            session.id
        )
        .as_bytes(),
    );
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(&mismatched, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(
        responses,
        vec![sasl_failure_xml(SaslFailureCondition::InvalidAuthzid)]
    );
    assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
    assert_eq!(
        *records.lock().expect("auth records"),
        vec![(
            waddle_xmpp::prometheus::AuthMechanism::OAuthBearer,
            waddle_xmpp::prometheus::AuthTerminalOutcome::InvalidCredentials,
        )]
    );

    let expected = localpart_to_jid(&session.xmpp_localpart, &state.deps.auth_state.xmpp_domain)
        .expect("session JID");
    let matching = oauthbearer_auth_frame(
        format!("n,a={expected},\x01auth=Bearer {}\x01\x01", session.id).as_bytes(),
    );
    let mut matching_conn = WsConnState::new();
    let responses =
        handle_xmpp_frame(&matching, "example.com", state.as_ref(), &mut matching_conn).await;
    assert_eq!(responses, vec![sasl_success_xml()]);
}

#[tokio::test]
async fn oauthbearer_is_unavailable_when_public_transport_is_insecure() {
    let mut state = create_test_websocket_state().await;
    Arc::get_mut(&mut state)
        .expect("unshared test state")
        .deps
        .oauthbearer_available = false;
    let mut conn = WsConnState::new();

    let open = handle_xmpp_frame(
        r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="example.com" version="1.0"/>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(!open[1].contains("OAUTHBEARER"));

    let auth = oauthbearer_auth_frame(b"n,,\x01auth=Bearer token\x01\x01");
    let responses = handle_xmpp_frame(&auth, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(
        responses,
        vec![sasl_failure_xml(SaslFailureCondition::InvalidMechanism)]
    );
}

#[tokio::test]
async fn sasl_abort_without_pending_exchange_is_rejected() {
    let state = create_test_websocket_state().await;
    let abort = element_to_xml(Element::builder("abort", waddle_xmpp::ns::SASL).build());
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(&abort, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(
        responses,
        vec![sasl_failure_xml(SaslFailureCondition::NotAuthorized)]
    );
    assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
}

#[tokio::test]
async fn oauthbearer_initial_response_distinguishes_encoding_from_malformed_auth() {
    let state = create_test_websocket_state().await;

    let invalid_base64 = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append("%%%not-base64")
            .build(),
    );
    let malformed = oauthbearer_auth_frame(b"not-an-rfc7628-response");

    for (frame, condition) in [
        (invalid_base64, SaslFailureCondition::IncorrectEncoding),
        (malformed, SaslFailureCondition::MalformedRequest),
    ] {
        let mut conn = WsConnState::new();
        let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;
        assert_eq!(responses, vec![sasl_failure_xml(condition)]);
        assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
    }
}

#[tokio::test]
async fn subsequent_auth_replaces_the_pending_oauthbearer_exchange() {
    let (state, records) = create_recording_oauth_test_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let auth = oauthbearer_auth_frame(b"n,,\x01auth=\x01\x01");
    let mut conn = WsConnState::new();

    let first = handle_xmpp_frame(&auth, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(first.len(), 1);
    assert!(conn.phase.has_pending_oauthbearer_exchange());

    let replacement =
        oauthbearer_auth_frame(format!("n,,\x01auth=Bearer {}\x01\x01", session.id).as_bytes());
    let replacement =
        handle_xmpp_frame(&replacement, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(replacement, vec![sasl_success_xml()]);
    assert!(conn.phase.is_authenticated());
    assert_eq!(
        *records.lock().expect("auth records"),
        vec![
            (
                waddle_xmpp::prometheus::AuthMechanism::OAuthBearer,
                waddle_xmpp::prometheus::AuthTerminalOutcome::Cancelled,
            ),
            (
                waddle_xmpp::prometheus::AuthMechanism::OAuthBearer,
                waddle_xmpp::prometheus::AuthTerminalOutcome::Success,
            ),
        ]
    );
}

#[tokio::test]
async fn malformed_xml_response_clears_oauthbearer_error_exchange() {
    let state = create_test_websocket_state().await;
    let auth = oauthbearer_auth_frame(b"n,,\x01auth=\x01\x01");
    let mut conn = WsConnState::new();

    let first = handle_xmpp_frame(&auth, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(first.len(), 1);
    assert!(conn.phase.has_pending_oauthbearer_exchange());

    let malformed = handle_xmpp_frame(
        r#"<response xmlns="urn:ietf:params:xml:ns:xmpp-sasl">not-closed"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert_eq!(
        malformed,
        vec![sasl_failure_xml(SaslFailureCondition::MalformedRequest)]
    );
    assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
}
