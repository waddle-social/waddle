use super::*;

fn disco_feature_vars_for_test(query: &Element) -> std::collections::BTreeSet<String> {
    query
        .children()
        .filter(|child| {
            child.name() == "feature" && child.ns() == waddle_xmpp::disco::DISCO_INFO_NS
        })
        .filter_map(|child| child.attr("var").map(str::to_string))
        .collect()
}

fn disco_items_for_test(query: &Element) -> Vec<(Option<String>, Option<String>)> {
    query
        .children()
        .filter(|child| child.name() == "item" && child.ns() == waddle_xmpp::disco::DISCO_ITEMS_NS)
        .map(|child| {
            (
                child.attr("jid").map(str::to_string),
                child.attr("node").map(str::to_string),
            )
        })
        .collect()
}

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

    assert_eq!(iq.id(), "roster-1");
    match iq.split().1 {
        xmpp_parsers::iq::IqPayload::Result(Some(payload)) => {
            assert_eq!(payload.name(), "query");
            assert_eq!(payload.ns(), "jabber:iq:roster");
        }
        _ => panic!("expected roster IQ result payload, got non-result"),
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
    // xmpp-parsers 0.22 tightened `Iq` to reject unknown attributes
    // (the derive uses `exhaustive`). An `<iq>` with a stray `data=…`
    // attribute is now rejected at the parse boundary, so the frame
    // never reaches the roster handler. RFC 6120 §8.2.3 allows
    // receivers to drop stanzas with undefined attributes — the
    // pre-0.22 lenient behaviour was opt-in, not mandated.
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
    assert!(
        responses.is_empty(),
        "iq with unknown attribute is rejected by xmpp-parsers 0.22, no response is emitted: {responses:?}"
    );
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

    assert_eq!(iq.id(), "carbons-1");
    match iq.split().1 {
        xmpp_parsers::iq::IqPayload::Result(None) => {}
        _ => panic!("expected empty IQ result, got non-result"),
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

    assert_eq!(iq.id(), "unknown-1");
    assert_eq!(
        iq.from().map(ToString::to_string).as_deref(),
        Some("example.com")
    );
    assert_eq!(
        iq.to().map(ToString::to_string).as_deref(),
        Some("alice@example.com/web")
    );
    match iq.split().1 {
        xmpp_parsers::iq::IqPayload::Error(_) => {}
        _ => panic!("expected IQ error payload, got non-result"),
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
        response.contains("status='executing'") || response.contains("status='executing'"),
        "expected executing command response, got: {response}"
    );
    assert!(
        response.contains("sessionid='") || response.contains("sessionid='"),
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
        !response.contains("status='executing'") && !response.contains("status='executing'"),
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
        response.contains("type='result'") || response.contains("type='result'"),
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
    assert!(
        response.contains("push.example.com"),
        "expected Push Service in server disco#items: {response}"
    );
}

#[tokio::test]
async fn handle_iq_disco_info_push_service_reports_xep0357_pubsub_identity() {
    let state = create_test_websocket_state().await;
    let query = disco_info_iq_frame("push-info", "push.example.com", None);

    let responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    let response = responses.first().expect("push service disco response");

    let iq = parse_iq_for_test(response);
    let query = match iq.split().1 {
        IqPayload::Result(Some(payload)) => payload,
        _ => panic!("expected push service disco#info result, got non-result"),
    };
    assert_eq!(query.name(), "query");
    assert_eq!(query.ns(), waddle_xmpp::disco::DISCO_INFO_NS);
    assert!(
        query.children().any(|child| {
            child.name() == "identity"
                && child.ns() == waddle_xmpp::disco::DISCO_INFO_NS
                && child.attr("category") == Some("pubsub")
                && child.attr("type") == Some("push")
        }),
        "XEP-0357 requires pubsub/push identity: {response}"
    );
    let features = disco_feature_vars_for_test(&query);
    assert!(
        features.contains("urn:xmpp:push:0"),
        "XEP-0357 requires urn:xmpp:push:0 feature: {response}"
    );
    assert!(
        features.contains("http://jabber.org/protocol/pubsub#publish"),
        "Push Service must advertise PubSub publish support: {response}"
    );
    assert!(
        features.contains("http://jabber.org/protocol/pubsub#access-whitelist"),
        "Push Service must advertise the XEP-0357 whitelist access profile: {response}"
    );
    assert!(
        features.contains("http://jabber.org/protocol/pubsub#publish-only-affiliation"),
        "Push Service must advertise the XEP-0357 publish-only affiliation profile: {response}"
    );
}

#[tokio::test]
async fn handle_iq_disco_items_push_service_is_owner_scoped() {
    let state = create_test_websocket_state().await;
    let alice: BareJid = "alice@example.com".parse().expect("alice");
    let bob: BareJid = "bob@example.com".parse().expect("bob");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let alice_node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&alice, "private-app")
        .await
        .expect("alice push node");
    let bob_node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&bob, "web")
        .await
        .expect("bob push node");
    let query = disco_items_iq_frame("push-items", "push.example.com", None);

    let unauth_responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;
    let unauth_response = unauth_responses.first().expect("unauth items");
    let unauth_iq = parse_iq_for_test(unauth_response);
    let unauth_query = match unauth_iq.split().1 {
        IqPayload::Result(Some(payload)) => payload,
        _ => panic!("expected unauth disco#items result, got non-result"),
    };
    assert!(disco_items_for_test(&unauth_query).is_empty());

    let alice_responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&alice_jid),
    )
    .await;
    let alice_response = alice_responses.first().expect("alice items");
    let alice_iq = parse_iq_for_test(alice_response);
    let alice_query = match alice_iq.split().1 {
        IqPayload::Result(Some(payload)) => payload,
        _ => panic!("expected alice disco#items result, got non-result"),
    };
    let alice_items = disco_items_for_test(&alice_query);
    assert_eq!(
        alice_items,
        vec![(
            Some("push.example.com".to_string()),
            Some(alice_node.node().to_string())
        )]
    );

    let bob_responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&bob_jid),
    )
    .await;
    let bob_response = bob_responses.first().expect("bob items");
    let bob_iq = parse_iq_for_test(bob_response);
    let bob_query = match bob_iq.split().1 {
        IqPayload::Result(Some(payload)) => payload,
        _ => panic!("expected bob disco#items result, got non-result"),
    };
    let bob_items = disco_items_for_test(&bob_query);
    assert_eq!(
        bob_items,
        vec![(
            Some("push.example.com".to_string()),
            Some(bob_node.node().to_string())
        )]
    );
}

#[tokio::test]
async fn handle_iq_disco_info_push_node_is_owner_scoped() {
    let state = create_test_websocket_state().await;
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let alice_jid: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob_jid: FullJid = "bob@example.com/web".parse().expect("bob jid");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&owner, "private-app-id")
        .await
        .expect("push node");
    let query = disco_info_iq_frame("push-node-info", "push.example.com", Some(node.node()));

    let bob_responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&bob_jid),
    )
    .await;
    let bob_response = bob_responses.first().expect("bob node info");
    assert!(
        bob_response.contains("item-not-found"),
        "non-owner must not discover push node metadata: {bob_response}"
    );

    let alice_responses = handle_iq(
        &query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&alice_jid),
    )
    .await;
    let alice_response = alice_responses.first().expect("alice node info");
    let alice_iq = parse_iq_for_test(alice_response);
    let query = match alice_iq.split().1 {
        IqPayload::Result(Some(payload)) => payload,
        _ => panic!("expected owner push node disco#info result, got non-result"),
    };
    assert_eq!(query.name(), "query");
    assert_eq!(query.ns(), waddle_xmpp::disco::DISCO_INFO_NS);
    assert!(
        !alice_response.contains("private-app-id"),
        "node disco#info must not leak app metadata: {alice_response}"
    );
    assert!(
        query.children().any(|child| {
            child.name() == "identity"
                && child.ns() == waddle_xmpp::disco::DISCO_INFO_NS
                && child.attr("category") == Some("pubsub")
                && child.attr("type") == Some("leaf")
        }),
        "push node disco#info must identify as a PubSub leaf: {alice_response}"
    );
    let features = disco_feature_vars_for_test(&query);
    for feature in [
        "http://jabber.org/protocol/disco#info",
        "http://jabber.org/protocol/pubsub",
        "http://jabber.org/protocol/pubsub#publish",
        "http://jabber.org/protocol/pubsub#access-whitelist",
        "http://jabber.org/protocol/pubsub#publish-only-affiliation",
        waddle_xmpp::xep::xep0357::NS_PUSH,
    ] {
        assert!(
            features.contains(feature),
            "push node disco#info missing required feature {feature}: {alice_response}"
        );
    }
}

#[tokio::test]
async fn handle_iq_push_service_custom_registration_keeps_provider_tokens_inside_service() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let ensure = Element::builder("ensure-node", crate::push_service::WADDLE_PUSH_SERVICE_NS)
        .attr(minidom::rxml::xml_ncname!("app-id").to_owned(), "web")
        .build();
    let responses = handle_iq(
        &iq_set_frame("push-node-1", "push.example.com", ensure),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    let node_iq = parse_iq_for_test(responses.first().expect("node response"));
    let node_id = match node_iq.split().1 {
        IqPayload::Result(Some(payload)) => payload.attr("id").expect("node id").to_string(),
        _ => panic!("expected node result, got non-result"),
    };

    let register = Element::builder(
        "register-device",
        crate::push_service::WADDLE_PUSH_SERVICE_NS,
    )
    .attr(
        minidom::rxml::xml_ncname!("node").to_owned(),
        node_id.as_str(),
    )
    .attr(minidom::rxml::xml_ncname!("device-id").to_owned(), "web-1")
    .attr(minidom::rxml::xml_ncname!("platform").to_owned(), "web")
    .attr(minidom::rxml::xml_ncname!("environment").to_owned(), "test")
    .append(
        Element::builder(
            "provider-endpoint",
            crate::push_service::WADDLE_PUSH_SERVICE_NS,
        )
        .append("https://push.example.com/endpoint")
        .build(),
    )
    .append(
        Element::builder(
            "provider-token",
            crate::push_service::WADDLE_PUSH_SERVICE_NS,
        )
        .append("provider-secret")
        .build(),
    )
    .append(
        Element::builder(
            "provider-key-material",
            crate::push_service::WADDLE_PUSH_SERVICE_NS,
        )
        .append("provider-key")
        .build(),
    )
    .build();
    let responses = handle_iq(
        &iq_set_frame("push-device-1", "push.example.com", register),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    let device_iq = parse_iq_for_test(responses.first().expect("device response"));
    match device_iq.split().1 {
        IqPayload::Result(Some(payload)) => {
            assert_eq!(payload.name(), "device");
            assert_eq!(payload.attr("status"), Some("active"));
        }
        _ => panic!("expected device result, got non-result"),
    }

    let device = state
        .deps
        .protocol
        .push_service
        .get_device_for_owner(&"alice@example.com".parse().expect("owner"), "web-1")
        .await
        .expect("device lookup")
        .expect("device");
    assert_eq!(
        device.provider_endpoint(),
        Some("https://push.example.com/endpoint")
    );
    assert_eq!(device.provider_token(), Some("provider-secret"));
    assert_eq!(device.provider_key_material(), Some("provider-key"));
}

#[tokio::test]
async fn handle_iq_push_service_disable_device_is_node_scoped() {
    let state = create_test_websocket_state().await;
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let first_node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&owner, "web")
        .await
        .expect("first node");
    let second_node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&owner, "mobile")
        .await
        .expect("second node");
    for node in [first_node.node(), second_node.node()] {
        state
            .deps
            .protocol
            .push_service
            .upsert_device(
                &owner,
                crate::push_service::PushDeviceRegistration::new(
                    "shared-device",
                    node,
                    crate::push_service::PushDevicePlatform::Web,
                    "test",
                ),
            )
            .await
            .expect("device");
    }

    let disable = Element::builder(
        "disable-device",
        crate::push_service::WADDLE_PUSH_SERVICE_NS,
    )
    .attr(
        minidom::rxml::xml_ncname!("node").to_owned(),
        first_node.node(),
    )
    .attr(
        minidom::rxml::xml_ncname!("device-id").to_owned(),
        "shared-device",
    )
    .build();
    let responses = handle_iq(
        &iq_set_frame("push-disable-1", "push.example.com", disable),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    let response_iq = parse_iq_for_test(responses.first().expect("disable response"));
    match response_iq.split().1 {
        IqPayload::Result(Some(payload)) => {
            assert_eq!(payload.name(), "device");
            assert_eq!(payload.attr("node"), Some(first_node.node()));
            assert_eq!(payload.attr("status"), Some("disabled"));
        }
        _ => panic!("expected disable-device result, got non-result"),
    }

    let first_publish = state
        .deps
        .protocol
        .push_service
        .publish_notification_from_user_server(
            first_node.node(),
            &waddle_xmpp::pubsub::PubSubItem::new(
                Some("first".to_string()),
                Some(Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build()),
            ),
            &owner,
        )
        .await
        .expect("first publish");
    let second_publish = state
        .deps
        .protocol
        .push_service
        .publish_notification_from_user_server(
            second_node.node(),
            &waddle_xmpp::pubsub::PubSubItem::new(
                Some("second".to_string()),
                Some(Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build()),
            ),
            &owner,
        )
        .await
        .expect("second publish");

    assert_eq!(first_publish.attempted_devices(), 0);
    assert_eq!(second_publish.attempted_devices(), 1);
}

#[tokio::test]
async fn handle_iq_xep0357_disable_removes_registration_without_retiring_push_service_node() {
    let state = create_test_websocket_state().await;
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&owner, "web")
        .await
        .expect("push node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &owner,
            crate::push_service::PushDeviceRegistration::new(
                "web-1",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            )
            .with_provider_token(Some("provider-secret".to_string())),
        )
        .await
        .expect("device");
    state
        .deps
        .protocol
        .push_store
        .register(waddle_xmpp::push::PushSubscription {
            user_jid: owner.to_string(),
            service_jid: "push.example.com".to_string(),
            node: Some(node.node().to_string()),
            publish_options: None,
            endpoint: None,
            p256dh: None,
            auth_key: None,
        })
        .await
        .expect("push registration");

    let disable = Element::builder("disable", waddle_xmpp::xep::xep0357::NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.node())
        .build();
    let responses = handle_iq(
        &iq_set_frame("xep0357-disable-first-party", "example.com", disable),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    let response_iq = parse_iq_for_test(responses.first().expect("disable response"));
    assert!(matches!(response_iq.split().1, IqPayload::Result(None)));

    let registrations = state
        .deps
        .protocol
        .push_store
        .get_for_user(&owner.to_string())
        .await
        .expect("push registrations after disable");
    let node_after_disable = state
        .deps
        .protocol
        .push_service
        .get_node_for_owner(&owner, node.node())
        .await
        .expect("node lookup after disable");
    let internal_publish = state
        .deps
        .protocol
        .push_service
        .publish_notification_from_user_server(
            node.node(),
            &waddle_xmpp::pubsub::PubSubItem::new(
                Some("disabled".to_string()),
                Some(Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build()),
            ),
            &owner,
        )
        .await
        .expect("Push Service provisioning remains usable after XEP-0357 disable");

    let reenabled_node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&owner, "web")
        .await
        .expect("reenabled node");
    let reenabled_publish = state
        .deps
        .protocol
        .push_service
        .publish_notification_from_user_server(
            reenabled_node.node(),
            &waddle_xmpp::pubsub::PubSubItem::new(
                Some("reenabled".to_string()),
                Some(Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build()),
            ),
            &owner,
        )
        .await
        .expect("reenabled publish");

    assert!(registrations.is_empty());
    assert!(node_after_disable.is_some());
    assert_eq!(internal_publish.attempted_devices(), 1);
    assert_eq!(reenabled_node.node(), node.node());
    assert_eq!(reenabled_publish.attempted_devices(), 1);
}

#[tokio::test]
async fn handle_iq_xep0357_disable_without_node_removes_registrations_only() {
    let state = create_test_websocket_state().await;
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let first_node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&owner, "web")
        .await
        .expect("first node");
    let second_node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&owner, "mobile")
        .await
        .expect("second node");
    for node in [first_node.node(), second_node.node()] {
        state
            .deps
            .protocol
            .push_service
            .upsert_device(
                &owner,
                crate::push_service::PushDeviceRegistration::new(
                    format!("device-{node}"),
                    node,
                    crate::push_service::PushDevicePlatform::Web,
                    "test",
                )
                .with_provider_token(Some("provider-secret".to_string())),
            )
            .await
            .expect("device");
        state
            .deps
            .protocol
            .push_store
            .register(waddle_xmpp::push::PushSubscription {
                user_jid: owner.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some(node.to_string()),
                publish_options: None,
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("push registration");
    }

    let disable = Element::builder("disable", waddle_xmpp::xep::xep0357::NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .build();
    let responses = handle_iq(
        &iq_set_frame("xep0357-disable-all-first-party", "example.com", disable),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    let response_iq = parse_iq_for_test(responses.first().expect("disable response"));
    assert!(matches!(response_iq.split().1, IqPayload::Result(None)));

    let items_response = handle_iq(
        &disco_items_iq_frame("push-items-after-disable", "push.example.com", None),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await
    .into_iter()
    .next()
    .expect("items response");

    assert!(items_response.contains(first_node.node()));
    assert!(items_response.contains(second_node.node()));
    assert!(state
        .deps
        .protocol
        .push_store
        .get_for_user(&owner.to_string())
        .await
        .expect("registrations")
        .is_empty());
}

#[tokio::test]
async fn handle_iq_xep0357_disable_without_matching_registration_does_not_retire_node() {
    let state = create_test_websocket_state().await;
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&owner, "web")
        .await
        .expect("node");

    let disable = Element::builder("disable", waddle_xmpp::xep::xep0357::NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            "push.example.com",
        )
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.node())
        .build();
    let responses = handle_iq(
        &iq_set_frame("xep0357-disable-unregistered", "example.com", disable),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    let response_iq = parse_iq_for_test(responses.first().expect("disable response"));
    assert!(matches!(response_iq.split().1, IqPayload::Result(None)));

    assert!(state
        .deps
        .protocol
        .push_service
        .get_node_for_owner(&owner, node.node())
        .await
        .expect("node lookup")
        .is_some());
}

#[tokio::test]
async fn handle_iq_pubsub_publish_to_push_service_rejects_client_origin_publish() {
    let state = create_test_websocket_state().await;
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&owner, "web")
        .await
        .expect("push node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &owner,
            crate::push_service::PushDeviceRegistration::new(
                "web-1",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("device");

    let notification = Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build();
    let item = Element::builder("item", waddle_xmpp::pubsub::NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "push-1")
        .append(notification)
        .build();
    let publish = Element::builder("publish", waddle_xmpp::pubsub::NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.node())
        .append(item)
        .build();
    let pubsub = Element::builder("pubsub", waddle_xmpp::pubsub::NS_PUBSUB)
        .append(publish)
        .build();

    let responses = handle_iq(
        &iq_set_frame("push-publish-1", "push.example.com", pubsub),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    let response_iq = parse_iq_for_test(responses.first().expect("publish response"));
    assert_eq!(response_iq.id(), "push-publish-1");
    match response_iq {
        Iq::Error { .. } => {}
        _ => panic!("expected PubSub publish error, got non-result"),
    }

    let attempts = state
        .deps
        .protocol
        .push_service
        .delivery_attempts_for_node(node.node())
        .await
        .expect("attempts");
    assert!(attempts.is_empty());
}

#[tokio::test]
async fn handle_iq_pubsub_publish_rejects_iq_get() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let notification = Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build();
    let item = Element::builder("item", waddle_xmpp::pubsub::NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "push-get")
        .append(notification)
        .build();
    let publish = Element::builder("publish", waddle_xmpp::pubsub::NS_PUBSUB)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "urn:xmpp:test",
        )
        .append(item)
        .build();
    let pubsub = Element::builder("pubsub", waddle_xmpp::pubsub::NS_PUBSUB)
        .append(publish)
        .build();
    let frame = stanza_to_xml(&Stanza::Iq(Box::new(Iq::Get {
        from: None,
        to: Some("alice@example.com".parse().expect("valid iq destination")),
        id: "pub-get".to_string(),
        payload: pubsub,
    })));

    let responses = handle_iq(
        &frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    let response = responses.first().expect("publish get response");

    assert!(
        response.contains("type='error'") && response.contains("bad-request"),
        "XEP-0060 publish must be IQ set: {response}"
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
        response.contains("type='result'") || response.contains("type='result'"),
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
    assert_eq!(iq.id(), "pub-1");
    match iq.split().1 {
        xmpp_parsers::iq::IqPayload::Result(Some(payload)) => {
            assert_eq!(payload.ns(), "http://jabber.org/protocol/pubsub");
        }
        _ => panic!("expected pubsub result, got non-result"),
    }
}

#[tokio::test]
async fn xep0402_bookmark_publish_updates_xep0492_projection() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let frame = r#"<iq xmlns="jabber:client" id="bookmark-notify-1" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="urn:xmpp:bookmarks:1">
                <item id="room@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1">
                        <extensions>
                            <notify xmlns="urn:xmpp:notification-settings:1"><never /></notify>
                        </extensions>
                    </conference>
                </item>
            </publish>
        </pubsub>
    </iq>"#;

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
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let conversation: BareJid = "room@muc.example.com".parse().expect("conversation");
    let projection = state
        .deps
        .protocol
        .notification_settings_projection
        .get(&owner, &conversation)
        .await
        .expect("projection lookup")
        .expect("projection row");
    assert_eq!(projection.mode, waddle_xmpp::xep::NotificationLevel::Never);
    assert_eq!(
        projection.conversation_kind,
        crate::notification_settings_projection::ConversationKind::PrivateGroup
    );
}

#[tokio::test]
async fn xep0402_bookmark_publish_overwrites_existing_xep0492_projection() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let first_frame = r#"<iq xmlns="jabber:client" id="bookmark-notify-overwrite-1" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="urn:xmpp:bookmarks:1">
                <item id="room@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1">
                        <extensions>
                            <notify xmlns="urn:xmpp:notification-settings:1"><never /></notify>
                        </extensions>
                    </conference>
                </item>
            </publish>
        </pubsub>
    </iq>"#;
    let second_frame = r#"<iq xmlns="jabber:client" id="bookmark-notify-overwrite-2" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="urn:xmpp:bookmarks:1">
                <item id="room@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1">
                        <extensions>
                            <notify xmlns="urn:xmpp:notification-settings:1"><on-mention /></notify>
                        </extensions>
                    </conference>
                </item>
            </publish>
        </pubsub>
    </iq>"#;

    let first_responses = handle_iq(
        first_frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert_eq!(
        first_responses.len(),
        1,
        "expected one response: {first_responses:?}"
    );

    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let conversation: BareJid = "room@muc.example.com".parse().expect("conversation");
    let first_projection = state
        .deps
        .protocol
        .notification_settings_projection
        .get(&owner, &conversation)
        .await
        .expect("first projection lookup")
        .expect("first projection row");

    let second_responses = handle_iq(
        second_frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert_eq!(
        second_responses.len(),
        1,
        "expected one response: {second_responses:?}"
    );

    let projection = state
        .deps
        .protocol
        .notification_settings_projection
        .get(&owner, &conversation)
        .await
        .expect("projection lookup")
        .expect("projection row");
    assert_eq!(
        projection.mode,
        waddle_xmpp::xep::NotificationLevel::OnMention
    );
    assert!(
        projection.source_version > first_projection.source_version,
        "projection source_version must advance on overwrite"
    );
}

#[tokio::test]
async fn xep0402_bookmark_publish_deletes_evicted_xep0492_projection() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let first_conversation: BareJid = "first@muc.example.com".parse().expect("first room");
    let second_conversation: BareJid = "second@muc.example.com".parse().expect("second room");
    let first_frame = r#"<iq xmlns="jabber:client" id="bookmark-notify-evict-1" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="urn:xmpp:bookmarks:1">
                <item id="first@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1">
                        <extensions>
                            <notify xmlns="urn:xmpp:notification-settings:1"><never /></notify>
                        </extensions>
                    </conference>
                </item>
            </publish>
        </pubsub>
    </iq>"#;
    let second_frame = r#"<iq xmlns="jabber:client" id="bookmark-notify-evict-2" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="urn:xmpp:bookmarks:1">
                <item id="second@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1">
                        <extensions>
                            <notify xmlns="urn:xmpp:notification-settings:1"><on-mention /></notify>
                        </extensions>
                    </conference>
                </item>
            </publish>
        </pubsub>
    </iq>"#;

    let first_responses = handle_iq(
        first_frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert_eq!(
        first_responses.len(),
        1,
        "expected one response: {first_responses:?}"
    );

    let mut node = state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&owner, waddle_xmpp::xep::xep0402::PEP_NODE)
        .await
        .expect("node lookup")
        .expect("bookmark node");
    node.config.max_items = 1;
    state
        .deps
        .protocol
        .pubsub_storage
        .update_node_config(&owner, waddle_xmpp::xep::xep0402::PEP_NODE, &node.config)
        .await
        .expect("update config");

    let second_responses = handle_iq(
        second_frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert_eq!(
        second_responses.len(),
        1,
        "expected one response: {second_responses:?}"
    );

    assert!(
        state
            .deps
            .protocol
            .notification_settings_projection
            .get(&owner, &first_conversation)
            .await
            .expect("first projection lookup")
            .is_none(),
        "retention-evicted bookmark must not leave a stale projection"
    );
    let second_projection = state
        .deps
        .protocol
        .notification_settings_projection
        .get(&owner, &second_conversation)
        .await
        .expect("second projection lookup")
        .expect("second projection row");
    assert_eq!(
        second_projection.mode,
        waddle_xmpp::xep::NotificationLevel::OnMention
    );
}

#[tokio::test]
async fn xep0402_bookmark_publish_without_notify_deletes_xep0492_projection() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let conversation: BareJid = "room@muc.example.com".parse().expect("conversation");
    state
        .deps
        .protocol
        .notification_settings_projection
        .upsert(
            &crate::notification_settings_projection::NotificationSettingsProjection {
                owner_bare_jid: owner.clone(),
                conversation_jid: conversation.clone(),
                conversation_kind:
                    crate::notification_settings_projection::ConversationKind::PrivateGroup,
                mode: waddle_xmpp::xep::NotificationLevel::Never,
                source_version: 1,
                updated_at_ms: 1,
                source: crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks,
                source_item_jid: conversation.clone(),
            },
        )
        .await
        .expect("seed projection");
    let frame = r#"<iq xmlns="jabber:client" id="bookmark-notify-2" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="urn:xmpp:bookmarks:1">
                <item id="room@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1" />
                </item>
            </publish>
        </pubsub>
    </iq>"#;

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
    assert!(
        responses[0].contains(r#"type='result'"#),
        "bookmark publish without notify should succeed before projection cleanup assertion: {}",
        responses[0]
    );
    assert!(state
        .deps
        .protocol
        .notification_settings_projection
        .get(&owner, &conversation)
        .await
        .expect("projection lookup")
        .is_none());
}

#[tokio::test]
async fn xep0402_bookmark_publish_with_malformed_notify_is_rejected() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let conversation: BareJid = "room@muc.example.com".parse().expect("conversation");
    let valid = r#"<iq xmlns="jabber:client" id="bookmark-malformed-seed" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="urn:xmpp:bookmarks:1">
                <item id="room@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1">
                        <extensions>
                            <notify xmlns="urn:xmpp:notification-settings:1"><never /></notify>
                        </extensions>
                    </conference>
                </item>
            </publish>
        </pubsub>
    </iq>"#;
    let _ = handle_iq(
        valid,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert!(state
        .deps
        .protocol
        .notification_settings_projection
        .get(&owner, &conversation)
        .await
        .expect("projection lookup")
        .is_some());

    let malformed = r#"<iq xmlns="jabber:client" id="bookmark-malformed-update" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="urn:xmpp:bookmarks:1">
                <item id="room@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1">
                        <extensions>
                            <notify xmlns="urn:xmpp:notification-settings:1">
                                <always />
                                <never />
                            </notify>
                        </extensions>
                    </conference>
                </item>
            </publish>
        </pubsub>
    </iq>"#;

    let responses = handle_iq(
        malformed,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(
        responses[0].contains(r#"type='error'"#) && responses[0].contains("bad-request"),
        "malformed XEP-0492 notify payload must be rejected: {}",
        responses[0]
    );
    assert!(state
        .deps
        .protocol
        .notification_settings_projection
        .get(&owner, &conversation)
        .await
        .expect("projection lookup")
        .is_some());
}

#[tokio::test]
async fn xep0402_bookmark_publish_with_duplicate_identity_notify_is_rejected() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let conversation: BareJid = "room@muc.example.com".parse().expect("conversation");
    let malformed = r#"<iq xmlns="jabber:client" id="bookmark-duplicate-identity" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="urn:xmpp:bookmarks:1">
                <item id="room@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1">
                        <extensions>
                            <notify xmlns="urn:xmpp:notification-settings:1">
                                <never identity-category="client" identity-type="pc" />
                                <never identity-category="client" identity-type="pc" />
                            </notify>
                        </extensions>
                    </conference>
                </item>
            </publish>
        </pubsub>
    </iq>"#;

    let responses = handle_iq(
        malformed,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(
        responses[0].contains(r#"type='error'"#) && responses[0].contains("bad-request"),
        "duplicate XEP-0492 identity settings must be rejected: {}",
        responses[0]
    );
    assert!(state
        .deps
        .protocol
        .notification_settings_projection
        .get(&owner, &conversation)
        .await
        .expect("projection lookup")
        .is_none());
}

#[tokio::test]
async fn xep0402_bookmark_publish_with_malformed_conference_is_rejected() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let malformed = r#"<iq xmlns="jabber:client" id="bookmark-malformed-conference" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="urn:xmpp:bookmarks:1">
                <item id="room@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1">
                        <unexpected xmlns="urn:xmpp:bookmarks:1" />
                    </conference>
                </item>
            </publish>
        </pubsub>
    </iq>"#;

    let responses = handle_iq(
        malformed,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(
        responses[0].contains(r#"type='error'"#) && responses[0].contains("bad-request"),
        "malformed XEP-0402 bookmark payload must be rejected: {}",
        responses[0]
    );
}

#[tokio::test]
async fn xep0402_bookmark_retract_deletes_xep0492_projection() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let conversation: BareJid = "room@muc.example.com".parse().expect("conversation");
    let publish = r#"<iq xmlns="jabber:client" id="bookmark-retract-pub" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="urn:xmpp:bookmarks:1">
                <item id="room@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1">
                        <extensions>
                            <notify xmlns="urn:xmpp:notification-settings:1"><never /></notify>
                        </extensions>
                    </conference>
                </item>
            </publish>
        </pubsub>
    </iq>"#;
    let _ = handle_iq(
        publish,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert!(state
        .deps
        .protocol
        .notification_settings_projection
        .get(&owner, &conversation)
        .await
        .expect("projection lookup")
        .is_some());

    let retract = r#"<iq xmlns="jabber:client" id="bookmark-retract-1" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <retract node="urn:xmpp:bookmarks:1">
                <item id="room@muc.example.com" />
            </retract>
        </pubsub>
    </iq>"#;

    let responses = handle_iq(
        retract,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(
        responses[0].contains(r#"type='result'"#),
        "bookmark retract should succeed before projection cleanup assertion: {}",
        responses[0]
    );
    assert!(state
        .deps
        .protocol
        .notification_settings_projection
        .get(&owner, &conversation)
        .await
        .expect("projection lookup")
        .is_none());
}

#[tokio::test]
async fn xep0402_bookmark_node_purge_deletes_xep0492_projection() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let conversation: BareJid = "room@muc.example.com".parse().expect("conversation");
    let publish = r#"<iq xmlns="jabber:client" id="bookmark-purge-pub" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="urn:xmpp:bookmarks:1">
                <item id="room@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1">
                        <extensions>
                            <notify xmlns="urn:xmpp:notification-settings:1"><never /></notify>
                        </extensions>
                    </conference>
                </item>
            </publish>
        </pubsub>
    </iq>"#;
    let _ = handle_iq(
        publish,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert!(state
        .deps
        .protocol
        .notification_settings_projection
        .get(&owner, &conversation)
        .await
        .expect("projection lookup")
        .is_some());

    let purge = r#"<iq xmlns="jabber:client" id="bookmark-purge-1" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub#owner">
            <purge node="urn:xmpp:bookmarks:1" />
        </pubsub>
    </iq>"#;

    let responses = handle_iq(
        purge,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(
        responses[0].contains(r#"type='result'"#),
        "bookmark purge should succeed before projection cleanup assertion: {}",
        responses[0]
    );
    assert!(state
        .deps
        .protocol
        .notification_settings_projection
        .get(&owner, &conversation)
        .await
        .expect("projection lookup")
        .is_none());
}

#[tokio::test]
async fn xep0402_bookmark_node_delete_deletes_xep0492_projection() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let conversation: BareJid = "room@muc.example.com".parse().expect("conversation");
    let publish = r#"<iq xmlns="jabber:client" id="bookmark-delete-pub" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="urn:xmpp:bookmarks:1">
                <item id="room@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1">
                        <extensions>
                            <notify xmlns="urn:xmpp:notification-settings:1"><never /></notify>
                        </extensions>
                    </conference>
                </item>
            </publish>
        </pubsub>
    </iq>"#;
    let _ = handle_iq(
        publish,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    assert!(state
        .deps
        .protocol
        .notification_settings_projection
        .get(&owner, &conversation)
        .await
        .expect("projection lookup")
        .is_some());

    let delete = r#"<iq xmlns="jabber:client" id="bookmark-delete-1" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub#owner">
            <delete node="urn:xmpp:bookmarks:1" />
        </pubsub>
    </iq>"#;

    let responses = handle_iq(
        delete,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    assert!(
        responses[0].contains(r#"type='result'"#),
        "bookmark delete should succeed before projection cleanup assertion: {}",
        responses[0]
    );
    assert!(state
        .deps
        .protocol
        .notification_settings_projection
        .get(&owner, &conversation)
        .await
        .expect("projection lookup")
        .is_none());
}

#[tokio::test]
async fn xep0402_bookmark_publish_and_retract_require_jid_item_ids() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let publish = r#"<iq xmlns="jabber:client" id="bookmark-invalid-id-publish" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="urn:xmpp:bookmarks:1">
                <item id="not-a-jid">
                    <conference xmlns="urn:xmpp:bookmarks:1" />
                </item>
            </publish>
        </pubsub>
    </iq>"#;

    let publish_responses = handle_iq(
        publish,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;

    assert_eq!(
        publish_responses.len(),
        1,
        "expected one response: {publish_responses:?}"
    );
    assert!(
        publish_responses[0].contains(r#"type='error'"#)
            && publish_responses[0].contains("bad-request"),
        "invalid bookmark item id must be rejected: {}",
        publish_responses[0]
    );

    let retract = r#"<iq xmlns="jabber:client" id="bookmark-invalid-id-retract" type="set">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <retract node="urn:xmpp:bookmarks:1">
                <item id="not-a-jid" />
            </retract>
        </pubsub>
    </iq>"#;

    let retract_responses = handle_iq(
        retract,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;

    assert_eq!(
        retract_responses.len(),
        1,
        "expected one response: {retract_responses:?}"
    );
    assert!(
        retract_responses[0].contains(r#"type='error'"#)
            && retract_responses[0].contains("bad-request"),
        "invalid bookmark retract item id must be rejected: {}",
        retract_responses[0]
    );
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
    assert_eq!(iq.id(), "items-1");
}
