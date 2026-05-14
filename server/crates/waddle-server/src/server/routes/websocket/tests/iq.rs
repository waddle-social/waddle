use super::*;

#[tokio::test]
async fn handle_iq_roster_query_returns_parseable_result() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let frame = r#"<iq xmlns="jabber:client" id="roster-1" type="get"><query xmlns="jabber:iq:roster"/></iq>"#;
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert_eq!(responses.len(), 1);

    let iq_xml = responses.first().expect("roster response");
    let element = Element::from_str(iq_xml).expect("valid IQ XML");
    let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");

    assert_eq!(iq.id, "roster-1");
    match iq.payload {
        xmpp_parsers::iq::IqType::Result(Some(payload)) => {
            assert_eq!(payload.name(), "query");
            assert_eq!(payload.ns(), "jabber:iq:roster");
        }
        other => panic!("expected roster IQ result payload, got {other:?}"),
    }
}

#[tokio::test]
async fn handle_xmpp_frame_roster_get_marks_connection_interested_for_detach() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid, false);
    let frame = r#"<iq xmlns="jabber:client" id="roster-interest" type="get"><query xmlns="jabber:iq:roster"/></iq>"#;

    let responses = handle_xmpp_frame(frame, "example.com", state.as_ref(), &mut conn).await;

    assert_eq!(responses.len(), 1);
    assert!(
        conn.roster_interested,
        "roster get must persist interest on WsConnState for SM detach"
    );
}

#[tokio::test]
async fn handle_iq_roster_query_without_xmlns_survives_xmlns_like_attribute_value() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let frame = r#"<iq id="roster-attr" type="get" data="xmlns=bogus"><query xmlns="jabber:iq:roster"/></iq>"#;
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert_eq!(responses.len(), 1);

    let iq_xml = responses.first().expect("roster response");
    let element = Element::from_str(iq_xml).expect("valid IQ XML");
    let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");
    assert_eq!(iq.id, "roster-attr");
    assert!(matches!(
        iq.payload,
        xmpp_parsers::iq::IqType::Result(Some(_))
    ));
}

#[tokio::test]
async fn handle_iq_roster_query_requires_ready_phase() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let frame = r#"<iq xmlns="jabber:client" id="roster-prebind" type="get"><query xmlns="jabber:iq:roster"/></iq>"#;

    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session.clone()),
        &authenticated_phase_for_session(&session, "example.com"),
    )
    .await;

    let response = responses.first().expect("roster auth error");
    assert!(
        response.contains("not-authorized"),
        "pre-bind roster should be rejected: {response}"
    );
    assert!(
        !response.contains("feature-not-implemented"),
        "pre-bind roster should not fall through as unimplemented: {response}"
    );
}

#[tokio::test]
async fn handle_iq_carbons_enable_returns_parseable_result() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let frame = r#"<iq xmlns="jabber:client" id="carbons-1" type="set"><enable xmlns="urn:xmpp:carbons:2"/></iq>"#;
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert_eq!(responses.len(), 1);

    let iq_xml = responses.first().expect("carbons response");
    let element = Element::from_str(iq_xml).expect("valid IQ XML");
    let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");

    assert_eq!(iq.id, "carbons-1");
    match iq.payload {
        xmpp_parsers::iq::IqType::Result(None) => {}
        other => panic!("expected empty IQ result, got {other:?}"),
    }
}

#[tokio::test]
async fn handle_iq_carbons_toggle_updates_registry_flag() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let (tx, _rx) = tokio::sync::mpsc::channel(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(jid.clone(), tx);
    assert!(!state
        .deps
        .protocol
        .connection_registry
        .is_carbons_enabled(&jid));

    let enable = r#"<iq xmlns="jabber:client" id="carbons-enable" type="set"><enable xmlns="urn:xmpp:carbons:2"/></iq>"#;
    let enable_responses = handle_iq(
        enable,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert_eq!(enable_responses.len(), 1);
    assert!(state
        .deps
        .protocol
        .connection_registry
        .is_carbons_enabled(&jid));

    let disable = r#"<iq xmlns="jabber:client" id="carbons-disable" type="set"><disable xmlns="urn:xmpp:carbons:2"/></iq>"#;
    let disable_responses = handle_iq(
        disable,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert_eq!(disable_responses.len(), 1);
    assert!(!state
        .deps
        .protocol
        .connection_registry
        .is_carbons_enabled(&jid));
}

#[tokio::test]
async fn handle_iq_unknown_includes_routing_addresses_in_error() {
    let state = create_test_websocket_state().await;
    let frame = r#"<iq xmlns="jabber:client" id="unknown-1" type="get" from="alice@example.com/web" to="example.com"><foo xmlns="urn:waddle:test:0"/></iq>"#;
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    assert_eq!(responses.len(), 1);

    let iq_xml = responses.first().expect("error response");
    let element = Element::from_str(iq_xml).expect("valid IQ XML");
    let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");

    assert_eq!(iq.id, "unknown-1");
    assert_eq!(
        iq.from.as_ref().map(ToString::to_string).as_deref(),
        Some("example.com")
    );
    assert_eq!(
        iq.to.as_ref().map(ToString::to_string).as_deref(),
        Some("alice@example.com/web")
    );
    match iq.payload {
        xmpp_parsers::iq::IqType::Error(_) => {}
        other => panic!("expected IQ error payload, got {other:?}"),
    }
}

#[tokio::test]
async fn handle_iq_result_returns_empty_response() {
    let state = create_test_websocket_state().await;
    let frame = r#"<iq xmlns="jabber:client" id="ack-1" type="result" from="alice@example.com/web" to="muc.example.com"/>"#;
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&sender_jid),
    )
    .await;
    assert!(
        responses.is_empty(),
        "IQ result should produce no response, got: {responses:?}"
    );
}

#[tokio::test]
async fn handle_iq_error_returns_empty_response() {
    let state = create_test_websocket_state().await;
    let frame = r#"<iq xmlns="jabber:client" id="err-1" type="error" from="alice@example.com/web" to="muc.example.com"><error type="cancel"><feature-not-implemented xmlns="urn:ietf:params:xml:ns:xmpp-stanzas"/></error></iq>"#;
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&sender_jid),
    )
    .await;
    assert!(
        responses.is_empty(),
        "IQ error should produce no response, got: {responses:?}"
    );
}

#[tokio::test]
async fn handle_xmpp_frame_server_iq_error_returns_empty_response() {
    let state = create_test_websocket_state().await;
    let mut conn = WsConnState::new();
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    conn.phase = ConnectionPhase::ready(sender_jid, false);

    let responses = handle_xmpp_frame(
            r#"<iq xmlns="jabber:client" from="waddle.social" id="016f8556-3f56-4a75-b159-ee0a1eb0823e" type="error"><error type="cancel"><feature-not-implemented xmlns="urn:ietf:params:xml:ns:xmpp-stanzas"/></error></iq>"#,
            "waddle.social",
            state.as_ref(),
            &mut conn,
        )
        .await;

    assert!(
        responses.is_empty(),
        "IQ error should produce no response, got: {responses:?}"
    );
}

#[tokio::test]
async fn handle_iq_command_request_routes_to_registry() {
    let state = create_test_websocket_state().await;
    state
        .deps
        .protocol
        .command_registry
        .register(
            "test:adhoc-command",
            "Test Command",
            |ctx: CommandContext| async move {
                CommandResult::Executing {
                    form: waddle_xmpp::xep::xep0004::DataForm::new(
                        waddle_xmpp::xep::xep0004::FormType::Form,
                    ),
                    session_id: ctx.command.session_id.unwrap_or_default(),
                    notes: vec![],
                }
            },
        )
        .await;

    let session = create_test_session(state.as_ref(), "alice").await;
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let frame = r#"<iq xmlns="jabber:client" id="cmd-1" type="set" to="example.com"><command xmlns="http://jabber.org/protocol/commands" node="test:adhoc-command" action="execute"/></iq>"#;
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&sender_jid),
    )
    .await;

    assert_eq!(responses.len(), 1);
    let response = responses.first().expect("command response");
    assert!(
        response.contains("status=\"executing\"") || response.contains("status='executing'"),
        "expected executing command response, got: {response}"
    );
    assert!(
        response.contains("sessionid=\"") || response.contains("sessionid='"),
        "expected command session ID in response, got: {response}"
    );
    assert!(
        !response.contains("feature-not-implemented"),
        "command IQ should not fall through to unhandled feature-not-implemented: {response}"
    );
}

#[tokio::test]
async fn handle_iq_command_request_requires_ready_phase() {
    let state = create_test_websocket_state().await;
    state
        .deps
        .protocol
        .command_registry
        .register(
            "test:adhoc-command",
            "Test Command",
            |_ctx: CommandContext| async move {
                CommandResult::Executing {
                    form: waddle_xmpp::xep::xep0004::DataForm::new(
                        waddle_xmpp::xep::xep0004::FormType::Form,
                    ),
                    session_id: String::new(),
                    notes: vec![],
                }
            },
        )
        .await;

    let session = create_test_session(state.as_ref(), "alice").await;
    let pending_jid: FullJid = "alice@example.com/pending".parse().expect("pending jid");
    let mut carbons_enabled = false;
    let mut roster_interested = false;
    let frame = r#"<iq xmlns="jabber:client" id="cmd-prebind-1" type="set" to="example.com"><command xmlns="http://jabber.org/protocol/commands" node="test:adhoc-command" action="execute"/></iq>"#;
    let mut conn_state = IqConnState {
        carbons_enabled: &mut carbons_enabled,
        roster_interested: &mut roster_interested,
        state_machine: None,
    };
    let responses = handle_iq_with_conn_state(
        parse_iq_for_test(frame),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ConnectionPhase::authenticated(&pending_jid),
        &mut conn_state,
    )
    .await;

    let response = responses.first().expect("command error response");
    assert!(
        response.contains("not-authorized"),
        "pre-bind command IQ should be rejected: {response}"
    );
    assert!(
        !response.contains("status=\"executing\"") && !response.contains("status='executing'"),
        "pre-bind command IQ must not reach the registry: {response}"
    );
}

#[tokio::test]
async fn handle_iq_disco_info_advertises_replies() {
    let server_domain = "example.com";
    let muc_domain = "muc.example.com";
    let state = create_test_websocket_state().await;

    let server_query = disco_info_iq_frame("srv1", "example.com", None);
    let server_responses = handle_iq(
        &server_query,
        server_domain,
        muc_domain,
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    let server_response = server_responses.first().expect("server disco response");
    assert!(server_response.contains("urn:xmpp:reply:0"));
    assert!(!server_response.contains("urn:xmpp:spaces:0"));
    assert!(!server_response.contains("urn:xmpp:fulltext:0"));
    assert!(!server_response.contains("urn:waddle:test-extension:1"));

    let muc_query = disco_info_iq_frame("muc1", "muc.example.com", None);
    let muc_responses = handle_iq(
        &muc_query,
        server_domain,
        muc_domain,
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    let muc_response = muc_responses.first().expect("muc disco response");
    assert!(muc_response.contains("urn:xmpp:reply:0"));
    assert!(!muc_response.contains("urn:waddle:test-extension:1"));

    let room_query = disco_info_iq_frame("room1", "room@muc.example.com", None);
    let room_responses = handle_iq(
        &room_query,
        server_domain,
        muc_domain,
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    let room_response = room_responses.first().expect("room disco response");
    assert!(room_response.contains("urn:xmpp:mam:2"));
    assert!(room_response.contains("urn:xmpp:reply:0"));
    assert!(room_response.contains("urn:xmpp:fulltext:0"));
    assert!(!room_response.contains("urn:waddle:test-extension:1"));

    let user_jid: FullJid = "alice@example.com/waddle".parse().expect("user jid");
    let user_query = disco_info_iq_frame("user1", "alice@example.com", None);
    let user_responses = handle_iq(
        &user_query,
        server_domain,
        muc_domain,
        state.as_ref(),
        &None,
        &ready_phase(&user_jid),
    )
    .await;
    let user_response = user_responses.first().expect("user disco response");
    assert!(user_response.contains("urn:xmpp:mam:2"));
    assert!(user_response.contains("urn:xmpp:fulltext:0"));
}

#[tokio::test]
async fn handle_iq_cross_user_pep_disco_resolves_session_backed_accounts() {
    let state = create_test_websocket_state().await;
    let alice = create_test_session(state.as_ref(), "alice-session").await;
    let bob = create_test_session(state.as_ref(), "bob-session").await;
    let bob_jid: FullJid = format!("{}@example.com/phone", bob.xmpp_localpart)
        .parse()
        .expect("bob jid");

    let query = disco_info_iq_frame("session-pep", "alice-session@example.com", None);
    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(bob),
        &ready_phase(&bob_jid),
    )
    .await;
    let response = responses.first().expect("session-backed PEP disco");

    assert!(
        response.contains("type=\"result\"") || response.contains("type='result'"),
        "session-backed user should expose PEP disco: {response}"
    );
    assert!(
        response.contains("http://jabber.org/protocol/pubsub#auto-create"),
        "expected PEP features for session-backed user: {response}"
    );
    assert!(
        !response.contains("urn:xmpp:mam:2"),
        "cross-user PEP disco must not expose personal MAM: {response}"
    );

    let missing_query = disco_info_iq_frame("session-pep-missing", "missing@example.com", None);
    let missing_responses = handle_iq(
        &missing_query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(alice),
        &ready_phase(
            &"alice-session@example.com/phone"
                .parse()
                .expect("alice jid"),
        ),
    )
    .await;
    let missing_response = missing_responses
        .first()
        .expect("missing session-backed PEP disco");
    assert!(
        missing_response.contains("item-not-found"),
        "unknown local user should not expose PEP disco: {missing_response}"
    );
}

#[tokio::test]
async fn handle_iq_disco_items_server_advertises_spaces_service() {
    let state = create_test_websocket_state().await;
    let query = disco_items_iq_frame("srv-items", "example.com", None);

    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    let response = responses.first().expect("server disco items response");

    assert!(
        response.contains("muc.example.com"),
        "expected MUC service: {response}"
    );
    assert!(
        response.contains("spaces.example.com"),
        "expected spaces service in server disco#items: {response}"
    );
}

#[tokio::test]
async fn handle_iq_disco_items_spaces_is_empty_without_owner_created_spaces() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;

    let authenticated_session = Some(session);
    let authenticated_jid: FullJid = format!(
        "{}@example.com/web",
        authenticated_session
            .as_ref()
            .expect("session")
            .xmpp_localpart
    )
    .parse()
    .expect("authenticated jid");
    let authenticated_phase = ready_phase(&authenticated_jid);
    let query = disco_items_iq_frame("spaces-items", "spaces.example.com", None);

    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &authenticated_session,
        &authenticated_phase,
    )
    .await;
    let response = responses.first().expect("spaces disco items response");

    assert!(
        !response.contains("node="),
        "fresh deployments must not advertise a synthetic space node: {response}"
    );
}

#[tokio::test]
async fn handle_iq_pubsub_items_spaces_node_lists_published_bookmarks() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;

    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "team")
        .await
        .expect("space node");
    let channel = waddle_xmpp::ChannelInfo {
        id: "general".to_string(),
        name: "General".to_string(),
        channel_type: "text".to_string(),
    };
    let item =
        waddle_xmpp::xep::build_channel_item(&channel, "muc.example.com").expect("bookmark item");
    state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces_jid, "team", &item, None, false)
        .await
        .expect("publish bookmark");

    let authenticated_session = Some(session);
    let authenticated_jid: FullJid = format!(
        "{}@example.com/web",
        authenticated_session
            .as_ref()
            .expect("session")
            .xmpp_localpart
    )
    .parse()
    .expect("authenticated jid");
    let authenticated_phase = ready_phase(&authenticated_jid);
    let query = r#"<iq xmlns="jabber:client" id="space-node-items" type="get" to="spaces.example.com"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="team"/></pubsub></iq>"#;

    let responses = handle_iq(
        query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &authenticated_session,
        &authenticated_phase,
    )
    .await;
    let response = responses
        .first()
        .expect("spaces node pubsub items response");

    assert!(
        response.contains("general@muc.example.com"),
        "expected channel room JID in spaces node pubsub items: {response}"
    );
    assert!(
        response.contains("conference") && response.contains("urn:xmpp:bookmarks:1"),
        "expected XEP-0402 conference item in spaces node pubsub items: {response}"
    );
    assert!(
        response.contains("General"),
        "expected channel name in spaces node pubsub items: {response}"
    );
}

#[tokio::test]
async fn handle_iq_disco_info_spaces_node_reports_open_for_public_space() {
    let state = create_test_websocket_state().await;
    let viewer = create_test_session(state.as_ref(), "viewer").await;
    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "team")
        .await
        .expect("space node");

    let viewer_phase = authenticated_phase_for_session(&viewer, "example.com");
    let query = disco_info_iq_frame("space-node-info", "spaces.example.com", Some("team"));
    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(viewer),
        &viewer_phase,
    )
    .await;
    let response = responses.first().expect("spaces node disco info response");

    assert!(
        response.contains("type=\"result\"") || response.contains("type='result'"),
        "expected successful node disco#info response: {response}"
    );
    assert!(
        response.contains("pubsub#access_model"),
        "expected access model metadata in node disco#info: {response}"
    );
    assert!(
        response.contains(">open<"),
        "expected public access model=open in metadata: {response}"
    );
}

#[tokio::test]
async fn handle_iq_disco_info_unknown_spaces_node_returns_item_not_found() {
    let state = create_test_websocket_state().await;
    let viewer = create_test_session(state.as_ref(), "viewer").await;

    let viewer_phase = authenticated_phase_for_session(&viewer, "example.com");
    let query = disco_info_iq_frame(
        "space-node-info-private",
        "spaces.example.com",
        Some("unknown"),
    );
    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(viewer),
        &viewer_phase,
    )
    .await;
    let response = responses
        .first()
        .expect("spaces node private disco info response");

    assert!(
        response.contains("item-not-found"),
        "unknown space node should not be discoverable: {response}"
    );
}

#[tokio::test]
async fn upload_slot_request_requires_ready_phase() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let pending_phase = authenticated_phase_for_session(&session, "example.com");
    let frame = r#"<iq xmlns='jabber:client' type='get' to='upload.example.com' id='upload-prebind-1'><request xmlns='urn:xmpp:http:upload:0' filename='hello.txt' size='5' content-type='text/plain'/></iq>"#;

    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &pending_phase,
    )
    .await;

    let response = responses.first().expect("upload error response");
    assert!(
        response.contains("not-authorized"),
        "pre-bind upload request should be rejected: {response}"
    );
}

#[tokio::test]
async fn handle_iq_pubsub_publish_returns_result() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let frame = r#"<iq xmlns="jabber:client" id="pub-1" type="set"><pubsub xmlns="http://jabber.org/protocol/pubsub"><publish node="http://jabber.org/protocol/mood"><item id="current"><mood xmlns="http://jabber.org/protocol/mood"><happy/></mood></item></publish></pubsub></iq>"#;
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    let element = Element::from_str(&responses[0]).expect("valid XML");
    let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");
    assert_eq!(iq.id, "pub-1");
    match iq.payload {
        xmpp_parsers::iq::IqType::Result(Some(payload)) => {
            assert_eq!(payload.ns(), "http://jabber.org/protocol/pubsub");
        }
        other => panic!("expected pubsub result, got {other:?}"),
    }
}

#[tokio::test]
async fn handle_iq_pubsub_items_empty_node_returns_result() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let frame = r#"<iq xmlns="jabber:client" id="items-1" type="get"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="http://jabber.org/protocol/mood"/></pubsub></iq>"#;
    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    let element = Element::from_str(&responses[0]).expect("valid XML");
    let iq = xmpp_parsers::iq::Iq::try_from(element).expect("parseable IQ");
    assert_eq!(iq.id, "items-1");
}
