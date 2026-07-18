use super::*;

#[tokio::test]
async fn handle_xmpp_frame_open_dispatches_via_typed_ingress() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="example.com" version="1.0"/>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 2);
    assert!(responses[0].contains("urn:ietf:params:xml:ns:xmpp-framing"));
    assert!(responses[1].contains("<features"));
}

#[tokio::test]
async fn handle_xmpp_frame_auth_dispatches_via_typed_ingress() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses, vec![sasl_success_xml()]);
    assert!(conn.phase.is_authenticated());
    assert!(conn.authenticated_session.is_some());
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_auth_returns_malformed_request() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<auth xmlns="urn:ietf:params:xml:ns:xmpp-sasl">payload</auth>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses, vec![sasl_failure_xml("malformed-request")]);
}

#[tokio::test]
async fn sync_state_machine_phase_mirrors_closing_into_sm() {
    // PR269 review fix #2/#6: when WsConnState.phase transitions
    // to Closing (via SASL failure / stream-error / explicit
    // shutdown inside `handle_xmpp_frame`), the per-connection SM
    // must mirror so it stops accepting late `PeerStanza`
    // dispatches from the outbound channel.
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: jid::FullJid = "alice@example.com/web".parse().expect("jid");
    conn.ensure_state_machine(
        "example.com",
        &state.deps.protocol.dispatcher,
        jid.clone(),
        false,
        Blocklist::empty(),
    );

    // Sanity: SM starts in Ready.
    assert!(matches!(
        conn.state_machine.as_ref().expect("sm").phase(),
        ConnectionPhase::Ready { .. }
    ));

    // Simulate the legacy phase tracker transitioning to Closing.
    conn.phase = ConnectionPhase::closing(Some(jid.clone()));
    conn.sync_state_machine_phase();

    assert!(matches!(
        conn.state_machine.as_ref().expect("sm").phase(),
        ConnectionPhase::Closing { .. }
    ));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_sasl_response_returns_malformed_request() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<response xmlns="urn:ietf:params:xml:ns:xmpp-sasl">not-closed"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses, vec![sasl_failure_xml("malformed-request")]);
}

#[tokio::test]
async fn handle_xmpp_frame_wrong_namespace_auth_stays_silent() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<auth xmlns="jabber:client" mechanism="SCRAM-SHA-256">x</auth>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(responses.is_empty(), "expected no response: {responses:?}");
}

#[tokio::test]
async fn websocket_features_advertise_oauthbearer() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="example.com" version="1.0"/>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 2);
    let features = &responses[1];
    assert!(
        features.contains("<mechanism>OAUTHBEARER</mechanism>"),
        "expected OAUTHBEARER in WebSocket SASL mechanisms"
    );
    assert!(
        features.contains("<mechanism>SCRAM-SHA-256</mechanism>"),
        "expected SCRAM-SHA-256 in WebSocket SASL mechanisms"
    );
    assert!(
        !features.contains("<mechanism>PLAIN</mechanism>"),
        "expected WebSocket SASL mechanisms to exclude PLAIN"
    );
}

#[tokio::test]
async fn websocket_close_moves_connection_into_closing_phase() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_single_websocket_close_frame(&responses);
    assert!(matches!(conn.phase, ConnectionPhase::Closing { .. }));
}

#[tokio::test]
async fn websocket_close_keeps_bound_connection_in_closing_phase() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    conn.phase = ConnectionPhase::ready(jid, false);

    let responses = handle_xmpp_frame(
        r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_single_websocket_close_frame(&responses);
    assert!(matches!(conn.phase, ConnectionPhase::Closing { .. }));

    let _ = handle_xmpp_frame(
        r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;
    assert!(matches!(conn.phase, ConnectionPhase::Closing { .. }));
}

fn assert_single_websocket_close_frame(responses: &[String]) {
    assert_eq!(responses.len(), 1);
    let close = Element::from_str(&responses[0]).expect("close frame xml");
    assert_eq!(close.name(), "close");
    assert_eq!(close.ns(), "urn:ietf:params:xml:ns:xmpp-framing");
}

#[tokio::test]
async fn websocket_rejects_plain_auth() {
    let state = create_test_websocket_state().await;
    let frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(minidom::rxml::xml_ncname!("mechanism").to_owned(), "PLAIN")
            .append(BASE64_STANDARD.encode("\0alice\0session-token"))
            .build(),
    );
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses, vec![sasl_failure_xml("invalid-mechanism")]);
    assert!(!conn.phase.is_authenticated());
    assert!(!conn.phase.is_ready());
    assert!(conn.authenticated_session.is_none());
}

#[tokio::test]
async fn websocket_oauthbearer_authenticates_session_token() {
    let state = create_test_websocket_state().await;
    let session = create_test_server_owner_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses, vec![sasl_success_xml()]);
    assert!(conn.phase.is_authenticated());
    assert!(!conn.phase.is_ready());
    assert_eq!(
        conn.authenticated_session
            .as_ref()
            .map(|s| s.user_jid.as_str()),
        Some(session.user_jid.as_str())
    );
    let expected_bare =
        localpart_to_jid(&session.xmpp_localpart, &state.deps.auth_state.xmpp_domain)
            .expect("session localpart should produce JID");
    assert_eq!(
        conn.phase.authenticated_bare_jid().map(ToString::to_string),
        Some(expected_bare)
    );
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
}

#[tokio::test]
async fn websocket_stream_open_sent_tracks_current_stream_instance() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let mut conn = WsConnState::new();
    let open =
        r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" to="example.com" version="1.0"/>"#;
    let close = r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#;

    assert!(!conn.stream_open_sent, "no stream answered yet at upgrade");
    handle_xmpp_frame(open, "example.com", state.as_ref(), &mut conn).await;
    assert!(conn.stream_open_sent, "server answered the first <open/>");

    // RFC 6120 §6.4.6: SASL success restarts the stream — the server
    // has not yet sent a response header for the NEW stream, so a
    // stream error is illegal in this gap.
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let responses = handle_xmpp_frame(&auth, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(responses, vec![sasl_success_xml()]);
    assert!(
        !conn.stream_open_sent,
        "SASL success restarts the stream; no response header exists for the new stream yet"
    );

    handle_xmpp_frame(open, "example.com", state.as_ref(), &mut conn).await;
    assert!(conn.stream_open_sent, "restarted stream answered");

    handle_xmpp_frame(close, "example.com", state.as_ref(), &mut conn).await;
    assert!(
        !conn.stream_open_sent,
        "a closed stream has no live response header to error on"
    );
}

#[tokio::test]
async fn websocket_rejects_reauthentication_after_successful_sasl() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let mut conn = WsConnState::new();

    let first = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(first, vec![sasl_success_xml()]);
    let first_bare_jid = conn.phase.authenticated_bare_jid().cloned();
    let first_user_id = conn
        .authenticated_session
        .as_ref()
        .map(|saved| saved.user_jid.clone());

    let second = handle_xmpp_frame(&frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(second, vec![sasl_failure_xml("not-authorized")]);
    assert!(conn.phase.is_authenticated());
    assert!(!conn.phase.is_ready());
    assert_eq!(conn.phase.authenticated_bare_jid(), first_bare_jid.as_ref());
    assert_eq!(
        conn.authenticated_session
            .as_ref()
            .map(|saved| saved.user_jid.clone()),
        first_user_id
    );
    assert!(matches!(conn.phase, ConnectionPhase::Authenticated { .. }));
}

#[tokio::test]
async fn websocket_failed_scram_response_resets_phase_to_unauthenticated() {
    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    register_test_native_user(state.as_ref(), "alice", "correct horse battery staple").await;
    let client_first = BASE64_STANDARD.encode("n,,n=alice,r=fyko+d2lbbFgONRv9qkxdawL");
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "SCRAM-SHA-256",
            )
            .append(client_first)
            .build(),
    );
    let response_frame = element_to_xml(
        Element::builder("response", waddle_xmpp::ns::SASL)
            .append(BASE64_STANDARD.encode("not-valid"))
            .build(),
    );
    let mut conn = WsConnState::new();

    let auth_responses = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses.len(), 1);
    let challenge = Element::from_str(&auth_responses[0]).expect("challenge xml");
    assert_eq!(challenge.name(), "challenge");
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

    let response_responses =
        handle_xmpp_frame(&response_frame, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(response_responses, vec![sasl_failure_xml("not-authorized")]);
    assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
    assert!(!conn.phase.is_authenticated());
    assert!(conn.authenticated_session.is_none());
}

#[tokio::test]
async fn websocket_malformed_scram_response_resets_phase_and_allows_retry() {
    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    register_test_native_user(state.as_ref(), "alice", "correct horse battery staple").await;
    let client_first = BASE64_STANDARD.encode("n,,n=alice,r=fyko+d2lbbFgONRv9qkxdawL");
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "SCRAM-SHA-256",
            )
            .append(client_first)
            .build(),
    );
    let malformed_response = r#"<response xmlns="urn:ietf:params:xml:ns:xmpp-sasl">not-closed"#;
    let mut conn = WsConnState::new();

    let first = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(first.len(), 1);
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

    let malformed = handle_xmpp_frame(malformed_response, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(malformed, vec![sasl_failure_xml("malformed-request")]);
    assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
    assert!(!conn.phase.is_authenticated());
    assert!(conn.authenticated_session.is_none());

    let retry = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(retry.len(), 1);
    let challenge = Element::from_str(&retry[0]).expect("challenge xml");
    assert_eq!(challenge.name(), "challenge");
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));
}

#[tokio::test]
async fn websocket_failed_reauth_during_scram_resets_phase_and_allows_retry() {
    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    register_test_native_user(state.as_ref(), "alice", "correct horse battery staple").await;
    let client_first = BASE64_STANDARD.encode("n,,n=alice,r=fyko+d2lbbFgONRv9qkxdawL");
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "SCRAM-SHA-256",
            )
            .append(client_first)
            .build(),
    );
    let mut conn = WsConnState::new();

    let first = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(first.len(), 1);
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

    let second = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(second, vec![sasl_failure_xml("not-authorized")]);
    assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));

    let third = handle_xmpp_frame(&auth_frame, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(third.len(), 1);
    let challenge = Element::from_str(&third[0]).expect("challenge xml");
    assert_eq!(challenge.name(), "challenge");
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));
}

#[tokio::test]
async fn different_auth_mechanism_aborting_scram_records_scram_failure_metrics() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let state = create_test_websocket_state().await;
    let domain = state.deps.auth_state.xmpp_domain.clone();
    register_test_native_user(state.as_ref(), "alice", "correct horse battery staple").await;
    let client_first = BASE64_STANDARD.encode("n,,n=alice,r=fyko+d2lbbFgONRv9qkxdawL");
    let scram_auth = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "SCRAM-SHA-256",
            )
            .append(client_first)
            .build(),
    );
    let different_auth = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(minidom::rxml::xml_ncname!("mechanism").to_owned(), "PLAIN")
            .append(BASE64_STANDARD.encode("\0alice\0irrelevant"))
            .build(),
    );
    let mut conn = WsConnState::new();

    let first = handle_xmpp_frame(&scram_auth, &domain, state.as_ref(), &mut conn).await;
    assert_eq!(first.len(), 1);
    assert_eq!(conn.phase.scram_pending_username(), Some("alice"));

    let second = handle_xmpp_frame(&different_auth, &domain, state.as_ref(), &mut conn).await;

    assert_eq!(second, vec![sasl_failure_xml("not-authorized")]);
    assert!(matches!(conn.phase, ConnectionPhase::Unauthenticated));
    assert_eq!(
        metrics.counter_sum(
            "waddle.auth.failures",
            &[("stage", "scram"), ("error_code", "other")]
        ),
        Some(1),
    );
    assert_eq!(
        metrics.counter_sum(
            "xmpp.auth.attempts",
            &[("mechanism", "SCRAM-SHA-256"), ("result", "failure")]
        ),
        Some(1),
    );
}

#[tokio::test]
async fn websocket_resource_bind_returns_client_iq() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let bind_frame = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "bind-1")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .append(
                Element::builder("bind", waddle_xmpp::ns::BIND)
                    .append(
                        Element::builder("resource", waddle_xmpp::ns::BIND)
                            .append("web")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    let mut conn = WsConnState::new();

    let auth_responses =
        handle_xmpp_frame(&auth_frame, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);

    let bind_responses =
        handle_xmpp_frame(&bind_frame, "example.com", state.as_ref(), &mut conn).await;

    assert!(conn.phase.is_ready());
    assert_eq!(bind_responses.len(), 1);

    let response = Element::from_str(&bind_responses[0]).expect("bind response XML");
    assert_eq!(response.name(), "iq");
    assert_eq!(response.ns(), waddle_xmpp::ns::JABBER_CLIENT);
    assert_eq!(response.attr("id"), Some("bind-1"));
    assert_eq!(response.attr("type"), Some("result"));

    let bind = response
        .get_child("bind", waddle_xmpp::ns::BIND)
        .expect("bind child");
    let jid = bind
        .get_child("jid", waddle_xmpp::ns::BIND)
        .expect("jid child");
    let expected_bare =
        localpart_to_jid(&session.xmpp_localpart, &state.deps.auth_state.xmpp_domain)
            .expect("session localpart should produce JID");
    let expected_full = format!("{expected_bare}/web");
    assert!(
        jid.text() == expected_full,
        "bound jid should match expected resource"
    );
    let bound_jid = conn.phase.bound_jid().map(ToString::to_string);
    assert!(
        bound_jid.as_deref() == Some(expected_full.as_str()),
        "connection state should store the bound jid"
    );
    assert!(matches!(
        &conn.phase,
        ConnectionPhase::Ready {
            full_jid,
            resumed: false,
            ..
        } if full_jid.to_string() == expected_full
    ));
}

#[tokio::test]
async fn websocket_resource_bind_without_resource_uses_unique_server_resource() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let bind_frame = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "bind-2")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .append(Element::builder("bind", waddle_xmpp::ns::BIND).build())
            .build(),
    );
    let mut conn = WsConnState::new();

    let auth_responses =
        handle_xmpp_frame(&auth_frame, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);

    let bind_responses =
        handle_xmpp_frame(&bind_frame, "example.com", state.as_ref(), &mut conn).await;

    assert!(conn.phase.is_ready());
    assert_eq!(bind_responses.len(), 1);

    let response = Element::from_str(&bind_responses[0]).expect("bind response XML");
    let bind = response
        .get_child("bind", waddle_xmpp::ns::BIND)
        .expect("bind child");
    let jid = bind
        .get_child("jid", waddle_xmpp::ns::BIND)
        .expect("jid child")
        .text();

    let expected_bare =
        localpart_to_jid(&session.xmpp_localpart, &state.deps.auth_state.xmpp_domain)
            .expect("session localpart should produce JID");
    let prefix = format!("{expected_bare}/ws-");
    assert!(
        jid.starts_with(&prefix),
        "server-assigned resource should be unique ws-* value: {jid}"
    );
    assert!(matches!(
        &conn.phase,
        ConnectionPhase::Ready {
            full_jid,
            resumed: false,
            ..
        } if full_jid.to_string() == jid
    ));
}

#[tokio::test]
async fn websocket_rejects_second_resource_bind_after_ready() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let payload = BASE64_STANDARD.encode(format!("n,,\x01auth=Bearer {}\x01\x01", session.id));
    let auth_frame = element_to_xml(
        Element::builder("auth", waddle_xmpp::ns::SASL)
            .attr(
                minidom::rxml::xml_ncname!("mechanism").to_owned(),
                "OAUTHBEARER",
            )
            .append(payload)
            .build(),
    );
    let bind_one = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "bind-1")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .append(
                Element::builder("bind", waddle_xmpp::ns::BIND)
                    .append(
                        Element::builder("resource", waddle_xmpp::ns::BIND)
                            .append("web")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    let bind_two = element_to_xml(
        Element::builder("iq", waddle_xmpp::ns::JABBER_CLIENT)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), "bind-2")
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .append(
                Element::builder("bind", waddle_xmpp::ns::BIND)
                    .append(
                        Element::builder("resource", waddle_xmpp::ns::BIND)
                            .append("mobile")
                            .build(),
                    )
                    .build(),
            )
            .build(),
    );
    let mut conn = WsConnState::new();

    let auth_responses =
        handle_xmpp_frame(&auth_frame, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(auth_responses, vec![sasl_success_xml()]);
    let bind_one_responses =
        handle_xmpp_frame(&bind_one, "example.com", state.as_ref(), &mut conn).await;
    assert_eq!(bind_one_responses.len(), 1);
    let first_bound_jid = conn.phase.bound_jid().cloned();

    let bind_two_responses =
        handle_xmpp_frame(&bind_two, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(
        bind_two_responses,
        vec![build_iq_error_xml_typed(
            "bind-2",
            None,
            None,
            not_authorized_iq_error("Authentication required."),
        )]
    );
    assert_eq!(conn.phase.bound_jid(), first_bound_jid.as_ref());
    assert!(matches!(conn.phase, ConnectionPhase::Ready { .. }));
}

#[tokio::test]
async fn handle_xmpp_frame_drops_oversized_input() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let huge = format!(
        "<iq id='big'>{}</iq>",
        "a".repeat(waddle_xmpp::protocol::frame::MAX_FRAME_SIZE)
    );
    let responses = handle_xmpp_frame(&huge, "example.com", state.as_ref(), &mut conn).await;
    assert!(responses.is_empty());
}

#[tokio::test]
async fn handle_xmpp_frame_drops_whitespace_padded_oversized_input() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let huge = format!(
        "{}<iq id='big'/>",
        " ".repeat(waddle_xmpp::protocol::frame::MAX_FRAME_SIZE)
    );
    let responses = handle_xmpp_frame(&huge, "example.com", state.as_ref(), &mut conn).await;
    assert!(responses.is_empty());
}

#[tokio::test]
async fn handle_xmpp_frame_invalid_iq_returns_feature_not_implemented() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq id="bad-iq" type="get"><nope/></iq>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='bad-iq'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_invalid_iq_result_returns_no_response() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq id="bad-result" type="result"><a/><b/></iq>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(responses.is_empty(), "expected no response: {responses:?}");
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_xml_iq_request_preserves_legacy_error() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq id="broken-iq" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='broken-iq'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_ignores_type_suffix_attributes() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq id="req-1" mimetype="result" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='req-1'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_recovers_attrs_with_spaces_and_gt() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq note="1 > 0" id = "req-2" type = "get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='req-2'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_skips_unquoted_attr_and_keeps_scanning() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq bogus=x id="req-3" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='req-3'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_skips_unquoted_attr_with_slashes() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq bogus=http://x id="req-4" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='req-4'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_keeps_id_after_empty_attr_value() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq bogus= id="req-5" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='req-5'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_recovers_unquoted_type_value() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq type=get id="req-6"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='req-6'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_wrong_namespace_iq_stays_silent() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
            r#"<iq xmlns="urn:ietf:params:xml:ns:xmpp-sasl" id="bad-ns" type="get"><ping xmlns="urn:xmpp:ping"/></iq>"#,
            "example.com",
            state.as_ref(),
            &mut conn,
        )
        .await;

    assert!(responses.is_empty(), "expected no response: {responses:?}");
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_self_closing_iq_result_stays_silent() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq type=result/>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(responses.is_empty(), "expected no response: {responses:?}");
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_skips_url_like_attr_with_equals() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq bogus=http://x=y id="req-7" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='req-7'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_does_not_shadow_real_type_attr() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq prev=type=result id="req-8" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='req-8'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_ignores_embedded_quoted_type_in_broken_value() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq prev=type="result" id="req-9" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='req-9'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_prefers_later_quoted_type_attr() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq type=result id="req-10" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='req-10'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_prefers_later_unquoted_type_attr() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq type=result type=get id="req-10b"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='req-10b'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_keeps_type_when_later_attr_is_truncated() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq type=get id="req-11" bogus="#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='req-11'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_keeps_unquoted_type_when_later_quote_is_unterminated() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq type=get id="req-12" to="alice@example.com"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains("type='error'"));
    assert!(responses[0].contains("id='req-12'"));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_unescapes_recovered_id() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq id="a&amp;b" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains(r#"id='a&amp;b'"#));
    assert!(!responses[0].contains(r#"id='a&amp;amp;b'"#));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_recovers_later_unquoted_type_attr() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq bogus= type=get id="req-13"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains(r#"id='req-13'"#));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_recovers_later_spaced_quoted_type_attr() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq bogus= type = "get" id="req-13b"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains(r#"id='req-13b'"#));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_does_not_treat_next_id_attr_as_type_value() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq type= id="req-14"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert!(responses.is_empty(), "expected no response: {responses:?}");
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_does_not_treat_next_type_attr_as_id_value() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq id= type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(!responses[0].contains(r#"id='type=&quot;get&quot;'"#));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_malformed_iq_keeps_invalid_numeric_entity_escaped() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();

    let responses = handle_xmpp_frame(
        r#"<iq id="&#1;" type="get"><ping xmlns="urn:xmpp:ping"></iq"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(responses[0].contains(r#"id='&amp;#1;'"#));
    assert!(responses[0].contains("feature-not-implemented"));
}

#[tokio::test]
async fn handle_xmpp_frame_ping_roundtrips_through_sans_io_path() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    conn.phase = ConnectionPhase::ready(jid, false);

    let responses = handle_xmpp_frame(
        r#"<iq id="ping-roundtrip" type="get"><ping xmlns="urn:xmpp:ping"/></iq>"#,
        "example.com",
        state.as_ref(),
        &mut conn,
    )
    .await;

    assert_eq!(responses.len(), 1);
    let element = Element::from_str(&responses[0]).expect("valid IQ XML");
    let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");
    assert_eq!(iq.id(), "ping-roundtrip");
    assert!(matches!(
        iq.split().1,
        xmpp_parsers::iq::IqPayload::Result(None)
    ));
}
