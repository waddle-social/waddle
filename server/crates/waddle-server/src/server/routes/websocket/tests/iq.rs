use super::*;
use crate::permissions::{CheckPermission, SubjectType};

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

async fn grant_space_member_for_test(state: &WebSocketState, space_node: &str, user_id: &str) {
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Space, space_node),
                Relation::new("member"),
                Subject::user(user_id),
            ),
        })
        .await
        .expect("space member tuple");
}

async fn channel_view_allowed_for_test(
    state: &WebSocketState,
    channel_id: &str,
    user_id: &str,
) -> bool {
    state
        .deps
        .app_state
        .permission_actor
        .ask(CheckPermission {
            subject: Subject::user(user_id),
            permission: Permission::View,
            object: Object::new(ObjectType::Channel, channel_id),
        })
        .await
        .expect("permission actor")
        .allowed
}

fn data_form_value_for_test(frame: &str, var: &str) -> Option<String> {
    let marker_single = format!("var='{var}'");
    let marker_double = format!("var=\"{var}\"");
    let idx = frame
        .find(&marker_single)
        .or_else(|| frame.find(&marker_double))?;
    let after = &frame[idx..];
    let open = after.find("<value>")?;
    let value = &after[open + "<value>".len()..];
    let close = value.find("</value>")?;
    Some(value[..close].to_string())
}

async fn link_preview_lookup_for_test(
    state: &WebSocketState,
    session: Session,
    bound_jid: &FullJid,
    frame: &str,
) -> Vec<String> {
    let mut carbons_enabled = false;
    let mut roster_interested = false;
    let mut blocklist_interested = false;
    let mut conn_state = IqConnState {
        carbons_enabled: &mut carbons_enabled,
        roster_interested: &mut roster_interested,
        blocklist_interested: &mut blocklist_interested,
        registry_owner: None,
        state_machine: None,
        ordered_relay_origin: None,
    };
    handle_iq_with_conn_state(
        parse_iq_for_test(frame),
        "example.com",
        "muc.example.com",
        state,
        &Some(session),
        &ready_phase(bound_jid),
        &mut conn_state,
    )
    .await
}

/// Register a live connection for `bound_jid` — with both the connection
/// registry and the authoritative `UserActor` the deferred delivery path
/// resolves (mirroring production registration) — so the deferred
/// link-preview reply (#1470) has somewhere to land; returns the outbound
/// receiver.
async fn register_link_preview_requester_for_test(
    state: &WebSocketState,
    bound_jid: &FullJid,
) -> tokio::sync::mpsc::Receiver<waddle_xmpp::registry::OutboundStanza> {
    let (tx, rx) = tokio::sync::mpsc::channel(8);
    state
        .deps
        .protocol
        .connection_registry
        .register(bound_jid.clone(), tx.clone());
    state
        .deps
        .protocol
        .user_registry
        .ask(waddle_xmpp::registry::RegisterUserResource {
            jid: bound_jid.clone(),
            entry: waddle_xmpp::registry::ConnectionEntry::new(tx),
        })
        .await
        .expect("register user resource");
    rx
}

/// Await the deferred link-preview IQ result (#1470) on the requester's
/// outbound channel and return it typed.
async fn recv_deferred_link_preview_reply_for_test(
    rx: &mut tokio::sync::mpsc::Receiver<waddle_xmpp::registry::OutboundStanza>,
) -> xmpp_parsers::iq::Iq {
    let outbound = tokio::time::timeout(std::time::Duration::from_secs(30), rx.recv())
        .await
        .expect("deferred lookup reply within timeout")
        .expect("outbound channel open");
    assert!(
        matches!(
            outbound.kind,
            waddle_xmpp::registry::DeliveryKind::DirectFrame
        ),
        "deferred IQ reply is a server-generated direct frame"
    );
    let Stanza::Iq(iq) = outbound.stanza else {
        panic!("expected deferred IQ reply, got {:?}", outbound.stanza);
    };
    *iq
}

#[tokio::test]
async fn link_preview_lookup_dispatch_returns_typed_unsupported_metadata_outcome() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let bound_jid: FullJid = "alice@example.com/desktop".parse().expect("jid");
    let mut rx = register_link_preview_requester_for_test(state.as_ref(), &bound_jid).await;
    let frame = "\
        <iq xmlns='jabber:client' type='get' id='preview-1' from='alice@example.com/desktop' to='example.com'>\
          <lookup xmlns='urn:waddle:link-preview:0'>\
            <url>https://example.com/article</url>\
            <scope>bob@example.com</scope>\
          </lookup>\
        </iq>";

    let responses = link_preview_lookup_for_test(state.as_ref(), session, &bound_jid, frame).await;

    assert!(
        responses.is_empty(),
        "accepted lookup answers off the dispatch path (#1470): {responses:?}"
    );
    let reply = recv_deferred_link_preview_reply_for_test(&mut rx).await;
    let xmpp_parsers::iq::Iq::Result {
        id,
        from,
        to,
        payload,
    } = reply
    else {
        panic!("normal resolver miss/failure is not an IQ transport error: {reply:?}");
    };
    assert_eq!(id, "preview-1", "preserves iq id");
    assert_eq!(
        from.as_ref().map(ToString::to_string).as_deref(),
        Some("example.com"),
        "stamps result 'from' from request envelope"
    );
    assert_eq!(
        to.as_ref().map(ToString::to_string).as_deref(),
        Some("alice@example.com/desktop"),
        "stamps result 'to' from request envelope"
    );
    let lookup = payload.expect("lookup payload");
    assert!(
        matches!(lookup.attr("status"), Some("unsupported" | "failed")),
        "normal resolver miss/failure is typed in the lookup result"
    );
    assert!(
        lookup.attr("token").is_none()
            && lookup
                .get_child("preview", waddle_xmpp::xep::NS_WADDLE_LINK_PREVIEW)
                .is_none(),
        "resolver miss/failure does not mint a token"
    );
}

#[tokio::test]
async fn link_preview_lookup_for_muc_scope_requires_current_occupant() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let bound_jid: FullJid = "alice@example.com/desktop".parse().expect("jid");
    let frame = "\
        <iq xmlns='jabber:client' type='get' id='preview-denied-1' from='alice@example.com/desktop' to='example.com'>\
          <lookup xmlns='urn:waddle:link-preview:0'>\
            <url>https://example.com/article</url>\
            <scope>private@muc.example.com</scope>\
          </lookup>\
        </iq>";

    let responses = link_preview_lookup_for_test(state.as_ref(), session, &bound_jid, frame).await;

    let response = responses.first().expect("lookup error");
    assert!(
        response.contains("id='preview-denied-1'"),
        "preserves iq id: {response}"
    );
    assert!(
        response.contains("type='error'"),
        "returns iq error: {response}"
    );
    assert!(
        response.contains("<forbidden"),
        "non-occupant MUC scope is forbidden: {response}"
    );
    assert!(
        !response.contains("token='"),
        "forbidden lookup must not mint token: {response}"
    );
}

#[tokio::test]
async fn link_preview_lookup_for_muc_scope_allows_current_occupant_before_resolver_outcome() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let bound_jid: FullJid = "alice@example.com/desktop".parse().expect("jid");
    let room_jid: jid::BareJid = "private@muc.example.com".parse().expect("room jid");
    let room_actor = get_or_create_room_actor(
        state.as_ref(),
        &room_jid,
        waddle_xmpp::muc::RoomConfig::default(),
        "space".to_string(),
        "private".to_string(),
    )
    .await
    .expect("create room")
    .actor_ref;
    room_actor
        .ask(JoinWithAffiliation {
            sender_jid: bound_jid.clone(),
            nick: "alice".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("join room");
    let mut rx = register_link_preview_requester_for_test(state.as_ref(), &bound_jid).await;
    let frame = "\
        <iq xmlns='jabber:client' type='get' id='preview-muc-1' from='alice@example.com/desktop' to='example.com'>\
          <lookup xmlns='urn:waddle:link-preview:0'>\
            <url>https://example.com/article</url>\
            <scope>private@muc.example.com</scope>\
          </lookup>\
        </iq>";

    let responses = link_preview_lookup_for_test(state.as_ref(), session, &bound_jid, frame).await;

    assert!(
        responses.is_empty(),
        "authorized lookup answers off the dispatch path (#1470): {responses:?}"
    );
    let reply = recv_deferred_link_preview_reply_for_test(&mut rx).await;
    let xmpp_parsers::iq::Iq::Result { payload, .. } = reply else {
        panic!("authorized room lookup should reach a typed resolver outcome: {reply:?}");
    };
    let lookup = payload.expect("lookup payload");
    assert!(
        matches!(lookup.attr("status"), Some("unsupported" | "failed")),
        "authorized room lookup should reach typed resolver outcome"
    );
    assert!(lookup
        .get_child("preview", waddle_xmpp::xep::NS_WADDLE_LINK_PREVIEW)
        .is_none());
}

/// Build a `<command/>` payload Element addressed at a XEP-0050
/// component. Wraps it in a `<x type='submit'>` data form when one is
/// provided so the call sites stay declarative.
fn command_iq_payload(
    node: &str,
    action: &str,
    session_id: Option<&str>,
    submit_form: Option<Element>,
) -> Element {
    let mut command = Element::builder("command", "http://jabber.org/protocol/commands")
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .attr(minidom::rxml::xml_ncname!("action").to_owned(), action);
    if let Some(session_id) = session_id {
        command = command.attr(
            minidom::rxml::xml_ncname!("sessionid").to_owned(),
            session_id,
        );
    }
    if let Some(form) = submit_form {
        command = command.append(form);
    }
    command.build()
}

/// Build a XEP-0004 `<x type='submit'>` Element pinning the given
/// `FORM_TYPE` and a list of `(var, value)` text-single fields. Keeps
/// the test sites focused on the field values rather than the XML
/// scaffolding around them.
fn xep0004_submit_form(form_type: &str, fields: &[(&str, &str)]) -> Element {
    const DATA_FORMS: &str = "jabber:x:data";
    let mut form = Element::builder("x", DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit");
    form = form.append(text_field(
        DATA_FORMS,
        "FORM_TYPE",
        form_type,
        Some("hidden"),
    ));
    for (var, value) in fields {
        form = form.append(text_field(DATA_FORMS, var, value, None));
    }
    form.build()
}

fn text_field(ns: &str, var: &str, value: &str, type_attr: Option<&str>) -> Element {
    let mut field =
        Element::builder("field", ns).attr(minidom::rxml::xml_ncname!("var").to_owned(), var);
    if let Some(type_attr) = type_attr {
        field = field.attr(minidom::rxml::xml_ncname!("type").to_owned(), type_attr);
    }
    field
        .append(Element::builder("value", ns).append(value).build())
        .build()
}

/// Look up a XEP-0004 field value by `var` inside a stage-4 result
/// form `<command><x type='result'>…</x></command>`. The XEP-0050
/// payload Element is the `<command/>` itself; this helper walks into
/// its `<x>` data-form child.
fn xep0004_field_value(command: &Element, var: &str) -> Option<String> {
    let form = command
        .children()
        .find(|child| child.is("x", "jabber:x:data"))?;
    form.children()
        .find(|child| child.is("field", "jabber:x:data") && child.attr("var") == Some(var))
        .and_then(|field| {
            field
                .children()
                .find(|child| child.is("value", "jabber:x:data"))
        })
        .map(|value| value.text())
}

/// Look up the first ACTIVE `push_devices` row registered against the
/// given (owner, node) pair via the test-only DB query helpers. Used
/// by sites that want to assert "a device exists" without knowing
/// the specific server-assigned `device_id`. Tests that DO care about
/// the assigned device id read it from the stage-4 result form
/// instead.
async fn first_active_device_for_owner_node(
    state: &WebSocketState,
    owner: &BareJid,
    node: &str,
) -> crate::push_service::PushServiceDevice {
    let devices = state
        .deps
        .protocol
        .push_service
        .test_only_active_devices_for_owner_node(owner, node)
        .await
        .expect("active devices");
    devices
        .into_iter()
        .next()
        .expect("at least one active device persisted")
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
                    actions: None,
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
async fn handle_iq_single_shot_completed_command_carries_generated_sessionid() {
    let state = create_test_websocket_state().await;
    state
        .deps
        .protocol
        .command_registry
        .register(
            "test:single-shot-command",
            "Single Shot Command",
            |_ctx: CommandContext| async move {
                CommandResult::Completed {
                    session_id: None,
                    form: None,
                    notes: vec![],
                }
            },
        )
        .await;

    let session = create_test_session(state.as_ref(), "alice").await;
    let sender_jid: FullJid = "alice@example.com/web".parse().expect("sender jid");
    let frame = r#"<iq xmlns="jabber:client" id="cmd-complete-1" type="set" to="example.com"><command xmlns="http://jabber.org/protocol/commands" node="test:single-shot-command" action="execute"/></iq>"#;
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
    let element = Element::from_str(response).expect("parse command response");
    assert_eq!(element.attr("type"), Some("result"));
    let command = element
        .children()
        .find(|child| child.name() == "command")
        .expect("command payload");
    assert_eq!(command.attr("status"), Some("completed"));
    let session_id = command.attr("sessionid").expect("completed sessionid");
    assert!(
        !session_id.is_empty(),
        "completed command sessionid must be non-empty: {response}"
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
                    actions: None,
                }
            },
        )
        .await;

    let session = create_test_session(state.as_ref(), "alice").await;
    let pending_jid: FullJid = "alice@example.com/pending".parse().expect("pending jid");
    let mut carbons_enabled = false;
    let mut roster_interested = false;
    let mut blocklist_interested = false;
    let frame = r#"<iq xmlns="jabber:client" id="cmd-prebind-1" type="set" to="example.com"><command xmlns="http://jabber.org/protocol/commands" node="test:adhoc-command" action="execute"/></iq>"#;
    let mut conn_state = IqConnState {
        carbons_enabled: &mut carbons_enabled,
        roster_interested: &mut roster_interested,
        blocklist_interested: &mut blocklist_interested,
        registry_owner: None,
        state_machine: None,
        ordered_relay_origin: None,
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
    assert!(muc_response.contains("http://jabber.org/protocol/disco#info"));
    assert!(muc_response.contains("http://jabber.org/protocol/disco#items"));
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
    // #1260 / XEP-0045 §6.4: disco#info on a room that does not exist
    // (no live actor, no persisted channel) is <item-not-found/>, never
    // a fabricated open-room response.
    let room_response = room_responses.first().expect("room disco response");
    assert!(room_response.contains("item-not-found"));
    assert!(!room_response.contains("urn:xmpp:mam:2"));
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
    // #1259 / XEP-0115 §5.4: the own-bare-JID response must be
    // well-formed — previously `urn:xmpp:mam:2`(+#extended) appeared
    // twice (explicit + via pep_features) and caps-verifying clients
    // discarded the whole response.
    let user_iq = Element::from_str(user_response).expect("user disco XML");
    let user_query = user_iq
        .get_child("query", waddle_xmpp::disco::DISCO_INFO_NS)
        .expect("user disco query");
    let parsed = waddle_xmpp::disco::info::parse_disco_info_response(user_query)
        .expect("parseable user disco#info");
    assert!(
        !parsed.ill_formed,
        "own-bare-JID disco#info must have no duplicate features/identities"
    );
    assert!(user_response.contains("urn:xmpp:fulltext:0"));
}

/// Regression test for #750: every component JID this deployment
/// advertises in disco#items MUST answer disco#info with a non-empty
/// IQ-result, identifying both its `category` AND `type`, in BOTH the
/// `Unauthenticated` and `ready` connection phases (the bound
/// resource shape the chat client uses after SASL+bind).
///
/// The component JIDs are derived from `state.deps.service_domains`
/// (the six XEP-0030 sub-domains) plus the XEP-0272 calls mixer
/// derived as `calls.<server-domain>` — same shape used by
/// `disco_items.rs:224` and `calls_mixer.rs:19`. Reading from the
/// shared config keeps this test in lock-step with the advertised
/// disco#items list; adding a service to `XmppServiceDomains` will
/// fail to compile here (the tuple list is exhaustive in spirit, not
/// in field destructuring, but the reviewer reading this is the
/// safety net) and the (category, type) expectation locks the
/// advertised identity shape so a swap of e.g. push from
/// `pubsub/push` to `pubsub/service` is caught here.
///
/// HAR captures against `waddle.chat` showed all 7 of these IQs
/// going unanswered in production. PR #749 added client-side
/// resilience; this test locks the server-side contract so a
/// regression to the silent-drop behavior is caught in CI.
#[tokio::test]
async fn handle_iq_disco_info_answers_every_component_domain() {
    let server_domain = "example.com";
    let muc_domain = "muc.example.com";
    let state = create_test_websocket_state().await;
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");

    let domains = &state.deps.service_domains;
    let calls_mixer = format!("calls.{server_domain}");
    let components: Vec<(&str, &str, &str)> = vec![
        // (jid, expected disco#info identity category, expected type)
        (domains.muc.as_str(), "conference", "text"),
        (domains.upload.as_str(), "store", "file"),
        (domains.spaces.as_str(), "pubsub", "service"),
        (domains.community.as_str(), "pubsub", "service"),
        (domains.extensions.as_str(), "pubsub", "service"),
        (domains.push.as_str(), "pubsub", "push"),
        (calls_mixer.as_str(), "conference", "audio-video"),
    ];

    for (target, expected_category, expected_type) in &components {
        for phase in [ConnectionPhase::Unauthenticated, ready_phase(&alice)] {
            let phase_label = match &phase {
                ConnectionPhase::Unauthenticated => "Unauthenticated",
                _ => "Ready",
            };
            let frame = disco_info_iq_frame(&format!("disco-{target}-{phase_label}"), target, None);
            let responses = handle_iq(
                &frame,
                server_domain,
                muc_domain,
                state.as_ref(),
                &None,
                &phase,
            )
            .await;
            assert_eq!(
                responses.len(),
                1,
                "disco#info to {target} in {phase_label} must produce exactly one frame: {responses:?}",
            );
            let xml = &responses[0];
            let element = Element::from_str(xml).unwrap_or_else(|err| {
                panic!("disco#info response for {target} must be valid XML: {err}\n{xml}")
            });
            let iq = xmpp_parsers::iq::Iq::try_from(element).unwrap_or_else(|err| {
                panic!("disco#info response for {target} must parse as IQ: {err}\n{xml}")
            });
            let query = match iq.split().1 {
                IqPayload::Result(Some(payload)) => payload,
                IqPayload::Error(_) => panic!(
                    "disco#info to {target} ({phase_label}) returned IqError instead of result\n{xml}",
                ),
                _ => panic!(
                    "disco#info to {target} ({phase_label}) returned non-result\n{xml}",
                ),
            };
            assert_eq!(
                query.ns(),
                waddle_xmpp::disco::DISCO_INFO_NS,
                "disco#info to {target} response must use the disco#info namespace: {xml}",
            );
            assert!(
                query.children().any(|child| child.name() == "identity"
                    && child.ns() == waddle_xmpp::disco::DISCO_INFO_NS
                    && child.attr("category") == Some(*expected_category)
                    && child.attr("type") == Some(*expected_type)),
                "disco#info to {target} ({phase_label}) must advertise identity \
                 category='{expected_category}' type='{expected_type}': {xml}",
            );
            assert!(
                query.children().any(|child| child.name() == "feature"
                    && child.ns() == waddle_xmpp::disco::DISCO_INFO_NS),
                "disco#info to {target} ({phase_label}) must advertise at least one feature: {xml}",
            );
        }
    }
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
async fn handle_iq_push_service_xep0050_registration_keeps_provider_tokens_inside_service() {
    // The XEP-0050 cutover replaces the custom `urn:waddle:push-service:0`
    // IQ shape with two ad-hoc commands at `push.<domain>`. Drive the
    // full multi-step dance and verify the persisted `push_devices` row
    // still carries the platform credentials inside the service
    // boundary — the chat client only ever sees the assigned node id in
    // the stage-4 result form, never the provider secrets.
    use crate::push_service::commands::{DISABLE_DEVICE_FORM_TYPE, REGISTER_DEVICE_FORM_TYPE};
    use crate::push_service::commands::{
        DISABLE_DEVICE_NODE, FIELD_APP_ID, FIELD_DEVICE_ID, FIELD_ENVIRONMENT, FIELD_NODE,
        FIELD_PLATFORM, FIELD_WEB_PUSH_AUTH, FIELD_WEB_PUSH_ENDPOINT, FIELD_WEB_PUSH_P256DH,
        REGISTER_DEVICE_NODE,
    };
    let _ = DISABLE_DEVICE_FORM_TYPE; // referenced via constant; silence unused-warning if disable test omits
    let _ = DISABLE_DEVICE_NODE;
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");

    // Stage 1 → 2: execute, expect status='executing' + sessionid +
    // form prompt back.
    let execute = command_iq_payload(REGISTER_DEVICE_NODE, "execute", None, None);
    let responses = handle_iq(
        &iq_set_frame("push-register-execute", "push.example.com", execute),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    let executing_iq = parse_iq_for_test(responses.first().expect("executing response"));
    let executing_payload = match executing_iq.split().1 {
        IqPayload::Result(Some(payload)) => payload,
        _ => panic!("expected executing command result"),
    };
    assert_eq!(executing_payload.attr("status"), Some("executing"));
    let session_id = executing_payload
        .attr("sessionid")
        .expect("XEP-0050 §3 sessionid")
        .to_string();

    // Stage 3 → 4: complete with the platform-discriminated submit
    // form, expect status='completed' + result form carrying `node`.
    let submit_form = xep0004_submit_form(
        REGISTER_DEVICE_FORM_TYPE,
        &[
            (FIELD_PLATFORM, "web"),
            (FIELD_ENVIRONMENT, "prod"),
            (FIELD_APP_ID, "web"),
            (FIELD_WEB_PUSH_ENDPOINT, "https://push.example.com/endpoint"),
            (FIELD_WEB_PUSH_P256DH, "provider-key"),
            (FIELD_WEB_PUSH_AUTH, "provider-secret"),
        ],
    );
    let complete = command_iq_payload(
        REGISTER_DEVICE_NODE,
        "complete",
        Some(&session_id),
        Some(submit_form),
    );
    let responses = handle_iq(
        &iq_set_frame("push-register-complete", "push.example.com", complete),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    let completed_iq = parse_iq_for_test(responses.first().expect("completed response"));
    let completed_payload = match completed_iq.split().1 {
        IqPayload::Result(Some(payload)) => payload,
        _ => panic!("expected completed command result"),
    };
    assert_eq!(completed_payload.attr("status"), Some("completed"));
    let assigned_node = xep0004_field_value(&completed_payload, FIELD_NODE)
        .expect("stage-4 result form carries `node`");
    assert!(!assigned_node.is_empty());

    // Headline invariant: the stage-4 result form returns ONLY the
    // assigned node id + device id — NEVER the provider credentials the
    // chat submitted. A regression that echoed creds back would leak
    // secrets to every entity that can observe the IQ result.
    let result_form = completed_payload
        .get_child("x", "jabber:x:data")
        .expect("stage-4 result form");
    let leaked_fields: Vec<String> = result_form
        .children()
        .filter(|child| child.name() == "field")
        .filter_map(|field| field.attr("var").map(str::to_string))
        .filter(|var| !matches!(var.as_str(), "FORM_TYPE" | FIELD_NODE | FIELD_DEVICE_ID))
        .collect();
    assert!(
        leaked_fields.is_empty(),
        "stage-4 result form leaked unexpected field(s): {leaked_fields:?}"
    );
    let form_values: Vec<String> = result_form
        .children()
        .filter(|child| child.name() == "field")
        .flat_map(|field| {
            field
                .children()
                .filter(|value| value.name() == "value")
                .map(|value| value.text())
        })
        .collect();
    for secret in [
        "provider-secret",
        "provider-key",
        "https://push.example.com/endpoint",
    ] {
        assert!(
            !form_values.iter().any(|value| value == secret),
            "stage-4 result form leaked credential `{secret}`: {form_values:?}"
        );
    }

    // The chat client never sees `device-id`, so look up the device by
    // the server-allocated node + owner pair.
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let device = first_active_device_for_owner_node(state.as_ref(), &owner, &assigned_node).await;
    assert_eq!(
        device.provider_endpoint(),
        Some("https://push.example.com/endpoint")
    );
    assert_eq!(device.provider_token(), Some("provider-secret"));
    assert_eq!(device.provider_key_material(), Some("provider-key"));
}

#[tokio::test]
async fn handle_iq_push_service_xep0050_disable_device_is_per_device_scoped() {
    // XEP-0050 `disable-device` carries BOTH the push node id and the
    // device id. The handler retires only the targeted `(node,
    // device_id)` row for the calling owner. Sibling devices on the
    // same node — and devices on other nodes — remain active. This
    // is the contract the chat's per-browser opt-out relies on:
    // unsubscribing one browser MUST NOT take down push for sibling
    // Apple / Android / other-browser installations on the same
    // `(user, app_id)` node.
    use crate::push_service::commands::{
        DISABLE_DEVICE_FORM_TYPE, DISABLE_DEVICE_NODE, FIELD_DEVICE_ID, FIELD_NODE,
    };

    let state = create_test_websocket_state().await;
    let owner: BareJid = "alice@example.com".parse().expect("owner");
    let jid: FullJid = "alice@example.com/web".parse().expect("valid jid");
    let shared_node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&owner, "web")
        .await
        .expect("shared node");
    // Two devices on the SAME node — the per-device opt-out must
    // disable only the targeted row.
    let device_a_id = "device-a";
    let device_b_id = "device-b";
    for device_id in [device_a_id, device_b_id] {
        state
            .deps
            .protocol
            .push_service
            .upsert_device(
                &owner,
                crate::push_service::PushDeviceRegistration::new(
                    device_id,
                    shared_node.node(),
                    crate::push_service::PushDevicePlatform::Web,
                    "test",
                ),
            )
            .await
            .expect("device");
    }

    let submit_form = xep0004_submit_form(
        DISABLE_DEVICE_FORM_TYPE,
        &[
            (FIELD_NODE, shared_node.node()),
            (FIELD_DEVICE_ID, device_a_id),
        ],
    );
    let disable = command_iq_payload(DISABLE_DEVICE_NODE, "execute", None, Some(submit_form));
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
    let completed_payload = match response_iq.split().1 {
        IqPayload::Result(Some(payload)) => payload,
        _ => panic!("expected disable-device result, got non-result"),
    };
    assert_eq!(completed_payload.attr("status"), Some("completed"));

    // The node is still active — the user-server XEP-0357
    // registration stays alive — and the sibling device row keeps
    // receiving fan-out. Publishing to the node MUST reach device B
    // (one attempted) and NOT device A (filtered to active rows).
    let publish = state
        .deps
        .protocol
        .push_service
        .publish_notification_from_user_server(
            shared_node.node(),
            &waddle_xmpp::pubsub::PubSubItem::new(
                Some("after-disable".to_string()),
                Some(Element::builder("notification", waddle_xmpp::xep::xep0357::NS_PUSH).build()),
            ),
            &owner,
        )
        .await
        .expect("publish still routes to sibling device");
    assert_eq!(
        publish.attempted_devices(),
        1,
        "exactly the sibling device remains active after per-device disable"
    );

    // Prove the RIGHT device survived: device-b is still active and
    // device-a is gone from the active set. A bug that disabled the
    // wrong row would also leave exactly one active device and slip past
    // the `attempted_devices() == 1` count above.
    let active = state
        .deps
        .protocol
        .push_service
        .test_only_active_devices_for_owner_node(&owner, shared_node.node())
        .await
        .expect("active devices");
    let active_ids: Vec<&str> = active.iter().map(|device| device.device_id()).collect();
    assert_eq!(
        active_ids,
        vec![device_b_id],
        "only the targeted device-a is disabled; the sibling device-b stays active"
    );
}

/// XEP-0050 `disable-device` is idempotent: a second call against an
/// already-disabled `(node, device_id)` row must surface
/// `status='completed'` (NOT a stanza error) so the chat's opt-out
/// retry path doesn't bounce on transient re-issue. The row still
/// matches the storage helper's `WHERE (node, device-id)` clause on the
/// second call, so it returns `Ok(())` and the handler maps that to
/// `Completed` the same as the first round. Round-3 adversarial
/// test-rigor finding; the post-loop assertion guards against the
/// second call resurrecting or otherwise mutating active state.
#[tokio::test]
async fn handle_iq_push_service_xep0050_disable_device_is_idempotent() {
    use crate::push_service::commands::{
        DISABLE_DEVICE_FORM_TYPE, DISABLE_DEVICE_NODE, FIELD_DEVICE_ID, FIELD_NODE,
    };

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
    let device_id = "device-idempotent";
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &owner,
            crate::push_service::PushDeviceRegistration::new(
                device_id,
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("device");

    let build_disable_payload = || {
        let submit_form = xep0004_submit_form(
            DISABLE_DEVICE_FORM_TYPE,
            &[(FIELD_NODE, node.node()), (FIELD_DEVICE_ID, device_id)],
        );
        command_iq_payload(DISABLE_DEVICE_NODE, "execute", None, Some(submit_form))
    };

    for round in ["first", "second"] {
        let responses = handle_iq(
            &iq_set_frame(
                &format!("push-disable-{round}"),
                "push.example.com",
                build_disable_payload(),
            ),
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &None,
            &ready_phase(&jid),
        )
        .await;
        let response_iq = parse_iq_for_test(responses.first().expect("disable response"));
        let completed_payload = match response_iq.split().1 {
            IqPayload::Result(Some(payload)) => payload,
            _ => panic!("expected disable-device result on {round} round"),
        };
        assert_eq!(
            completed_payload.attr("status"),
            Some("completed"),
            "{round} disable-device must complete (idempotent)"
        );
    }

    // No side effect from the second disable: the device stays disabled
    // (zero active rows), it was never resurrected.
    let active = state
        .deps
        .protocol
        .push_service
        .test_only_active_devices_for_owner_node(&owner, node.node())
        .await
        .expect("active devices");
    assert!(
        active.is_empty(),
        "double-disable must leave the device disabled, not resurrect it: {active:?}"
    );
}

/// XEP-0050 `disable-device` against a valid, owned, active node but an
/// unknown `device-id` is NOT a silent success — the storage helper
/// distinguishes "no such (node, device-id) row" from "row disabled"
/// and the handler surfaces `item-not-found` so a stale/typo'd
/// device-id is reported honestly (adversarial finding B2).
#[tokio::test]
async fn handle_iq_push_service_xep0050_disable_unknown_device_is_item_not_found() {
    use crate::push_service::commands::{
        DISABLE_DEVICE_FORM_TYPE, DISABLE_DEVICE_NODE, FIELD_DEVICE_ID, FIELD_NODE,
    };

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
    // A real device exists on the node, so the node is active — only the
    // submitted device-id is bogus.
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &owner,
            crate::push_service::PushDeviceRegistration::new(
                "real-device",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("device");

    let submit_form = xep0004_submit_form(
        DISABLE_DEVICE_FORM_TYPE,
        &[
            (FIELD_NODE, node.node()),
            (FIELD_DEVICE_ID, "never-registered"),
        ],
    );
    let disable = command_iq_payload(DISABLE_DEVICE_NODE, "execute", None, Some(submit_form));
    let responses = handle_iq(
        &iq_set_frame("push-disable-bogus", "push.example.com", disable),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    let response = responses.first().expect("disable response");
    let response_iq = parse_iq_for_test(response);
    assert!(
        matches!(response_iq.split().1, IqPayload::Error(_)),
        "disabling an unknown device-id must be an error, not a false success: {response}"
    );
    let element = Element::from_str(response).expect("parse error iq");
    let error = element
        .children()
        .find(|child| child.name() == "error")
        .expect("error envelope");
    assert!(
        error
            .children()
            .any(|child| child.name() == "item-not-found"),
        "unknown device-id must map to item-not-found: {response}"
    );
    // The real device is untouched — still active.
    let active = state
        .deps
        .protocol
        .push_service
        .test_only_active_devices_for_owner_node(&owner, node.node())
        .await
        .expect("active devices");
    let active_ids: Vec<&str> = active.iter().map(|device| device.device_id()).collect();
    assert_eq!(active_ids, vec!["real-device"]);
}

/// Authorization: a caller cannot `disable-device` a device on a push
/// node owned by ANOTHER user, even with a valid node + device-id. The
/// storage owner-gate rejects with `forbidden` and the victim's device
/// stays active. This pins the per-owner scoping invariant at the
/// command-handler layer (the prior suite only exercised it at the
/// storage layer with a single owner) — adversarial finding M3.
#[tokio::test]
async fn handle_iq_push_service_xep0050_disable_device_rejects_foreign_owner() {
    use crate::push_service::commands::{
        DISABLE_DEVICE_FORM_TYPE, DISABLE_DEVICE_NODE, FIELD_DEVICE_ID, FIELD_NODE,
    };

    let state = create_test_websocket_state().await;
    let alice: BareJid = "alice@example.com".parse().expect("alice");
    let alice_node = state
        .deps
        .protocol
        .push_service
        .ensure_node(&alice, "web")
        .await
        .expect("alice node");
    state
        .deps
        .protocol
        .push_service
        .upsert_device(
            &alice,
            crate::push_service::PushDeviceRegistration::new(
                "alice-device",
                alice_node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("device");

    // Bob is authenticated and submits Alice's node + device-id.
    let bob: FullJid = "bob@example.com/web".parse().expect("bob");
    let submit_form = xep0004_submit_form(
        DISABLE_DEVICE_FORM_TYPE,
        &[
            (FIELD_NODE, alice_node.node()),
            (FIELD_DEVICE_ID, "alice-device"),
        ],
    );
    let disable = command_iq_payload(DISABLE_DEVICE_NODE, "execute", None, Some(submit_form));
    let responses = handle_iq(
        &iq_set_frame("push-disable-foreign", "push.example.com", disable),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&bob),
    )
    .await;
    let response = responses.first().expect("disable response");
    let response_iq = parse_iq_for_test(response);
    assert!(
        matches!(response_iq.split().1, IqPayload::Error(_)),
        "foreign-owner disable must be rejected: {response}"
    );
    let element = Element::from_str(response).expect("parse error iq");
    let error = element
        .children()
        .find(|child| child.name() == "error")
        .expect("error envelope");
    assert!(
        error.children().any(|child| child.name() == "forbidden"),
        "foreign-owner disable must map to forbidden: {response}"
    );

    // Alice's device is untouched — still active.
    let active = state
        .deps
        .protocol
        .push_service
        .test_only_active_devices_for_owner_node(&alice, alice_node.node())
        .await
        .expect("active devices");
    let active_ids: Vec<&str> = active.iter().map(|device| device.device_id()).collect();
    assert_eq!(active_ids, vec!["alice-device"]);
}

/// XEP-0050 §4.4 wire shape on the live dispatch path: a `complete`
/// carrying a sessionid that was never issued is rejected with
/// `modify`/`<bad-request/>` PLUS the command-namespaced
/// `<bad-sessionid/>` specific condition. The conformant builders used
/// to exist only as dead code while the live path emitted a bare
/// `<bad-request/>`; this pins the application-condition child on the
/// real path (adversarial conformance finding B1).
#[tokio::test]
async fn handle_iq_push_service_xep0050_complete_with_unknown_sessionid_is_bad_sessionid() {
    use crate::push_service::commands::REGISTER_DEVICE_NODE;

    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    // `complete` referencing a sessionid the server never issued.
    let complete = command_iq_payload(
        REGISTER_DEVICE_NODE,
        "complete",
        Some("never-issued-session"),
        None,
    );
    let responses = handle_iq(
        &iq_set_frame("push-bad-session", "push.example.com", complete),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    let response = responses.first().expect("response");
    let element = Element::from_str(response).expect("parse error iq");
    assert_eq!(element.attr("type"), Some("error"));
    let error = element
        .children()
        .find(|child| child.name() == "error")
        .expect("error envelope");
    assert_eq!(error.attr("type"), Some("modify"));
    assert!(
        error.children().any(|child| child.name() == "bad-request"),
        "§4.4 general condition must be bad-request: {response}"
    );
    assert!(
        error.children().any(|child| {
            child.name() == "bad-sessionid"
                && child.ns() == waddle_xmpp::xep::xep0050::NS_COMMANDS
        }),
        "§4.4 must carry the <bad-sessionid xmlns='http://jabber.org/protocol/commands'/> child: {response}"
    );
}

/// XEP-0050 §4.4 wire shape on the live dispatch path: a command IQ
/// carrying an `action` the responder does not understand is rejected
/// with `modify`/`<bad-request/>` PLUS the command-namespaced
/// `<malformed-action/>` specific condition (adversarial re-review
/// finding — the `MalformedAction` variant was previously dead).
#[tokio::test]
async fn handle_iq_push_service_xep0050_unknown_action_is_malformed_action() {
    use crate::push_service::commands::REGISTER_DEVICE_NODE;

    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/web".parse().expect("jid");
    // `action='frobnicate'` is not a valid XEP-0050 action.
    let bogus = command_iq_payload(REGISTER_DEVICE_NODE, "frobnicate", None, None);
    let responses = handle_iq(
        &iq_set_frame("push-bad-action", "push.example.com", bogus),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&jid),
    )
    .await;
    let response = responses.first().expect("response");
    let element = Element::from_str(response).expect("parse error iq");
    assert_eq!(element.attr("type"), Some("error"));
    let error = element
        .children()
        .find(|child| child.name() == "error")
        .expect("error envelope");
    assert_eq!(error.attr("type"), Some("modify"));
    assert!(
        error.children().any(|child| child.name() == "bad-request"),
        "§4.4 general condition must be bad-request: {response}"
    );
    assert!(
        error.children().any(|child| {
            child.name() == "malformed-action"
                && child.ns() == waddle_xmpp::xep::xep0050::NS_COMMANDS
        }),
        "§4.4 must carry the <malformed-action xmlns='http://jabber.org/protocol/commands'/> child: {response}"
    );
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
    state
        .deps
        .protocol
        .pubsub_storage
        .update_node_config(
            &spaces_jid,
            "team",
            &waddle_xmpp::pubsub::NodeConfig::spaces_public(),
        )
        .await
        .expect("space node config");
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
async fn handle_iq_pubsub_items_spaces_node_backfills_linked_channel_bookmarks() {
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
    state
        .deps
        .protocol
        .pubsub_storage
        .update_node_config(
            &spaces_jid,
            "team",
            &waddle_xmpp::pubsub::NodeConfig::spaces_public(),
        )
        .await
        .expect("space node config");

    let channel_jid: BareJid = "legacy@muc.example.com".parse().expect("channel jid");
    let mut config = waddle_xmpp::muc::RoomConfig {
        name: "Legacy".to_string(),
        description: None,
        persistent: true,
        members_only: false,
        ..waddle_xmpp::muc::RoomConfig::default()
    };
    config.enable_logging = true;
    state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
            room_jid: channel_jid.clone(),
            waddle_id: "test".to_string(),
            channel_id: "legacy".to_string(),
            config,
        })
        .await
        .expect("create room");
    state
        .deps
        .app_state
        .channel_space_link_store
        .set(&crate::channel_space_links::ChannelSpaceLink {
            channel_jid: channel_jid.clone(),
            space_jid: "team@spaces.example.com".parse().expect("space jid"),
            space_node: crate::space_identity::SpaceNode::from("team"),
            created_at: 0,
        })
        .await
        .expect("link channel to space");

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
    let query = r#"<iq xmlns="jabber:client" id="space-node-items-backfill" type="get" to="spaces.example.com"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="team"/></pubsub></iq>"#;

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
        response.contains("legacy@muc.example.com"),
        "expected linked channel to be backfilled as XEP-0503 bookmark: {response}"
    );
    assert!(
        response.contains("Legacy"),
        "expected backfilled bookmark to use room config name: {response}"
    );
    let stored = state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&spaces_jid, "team", None, &[])
        .await
        .expect("stored backfilled bookmark");
    assert!(
        stored.iter().any(|item| item.id == channel_jid.to_string()),
        "backfill should persist the bookmark item"
    );
}

#[tokio::test]
async fn handle_iq_pubsub_items_spaces_node_requires_authorization_before_backfill() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "bob").await;

    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "private-team")
        .await
        .expect("space node");
    state
        .deps
        .protocol
        .pubsub_storage
        .update_node_config(
            &spaces_jid,
            "private-team",
            &waddle_xmpp::pubsub::NodeConfig::spaces_private(),
        )
        .await
        .expect("private space node config");

    let channel_jid: BareJid = "private-legacy@muc.example.com"
        .parse()
        .expect("channel jid");
    state
        .deps
        .protocol
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
            room_jid: channel_jid.clone(),
            waddle_id: "test".to_string(),
            channel_id: "private-legacy".to_string(),
            config: waddle_xmpp::muc::RoomConfig {
                name: "Private Legacy".to_string(),
                persistent: true,
                members_only: true,
                enable_logging: true,
                ..waddle_xmpp::muc::RoomConfig::default()
            },
        })
        .await
        .expect("create room");
    state
        .deps
        .app_state
        .channel_space_link_store
        .set(&crate::channel_space_links::ChannelSpaceLink {
            channel_jid: channel_jid.clone(),
            space_jid: "private-team@spaces.example.com"
                .parse()
                .expect("space jid"),
            space_node: crate::space_identity::SpaceNode::from("private-team"),
            created_at: 0,
        })
        .await
        .expect("link channel to space");

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
    let query = r#"<iq xmlns="jabber:client" id="space-node-items-private" type="get" to="spaces.example.com"><pubsub xmlns="http://jabber.org/protocol/pubsub"><items node="private-team"/></pubsub></iq>"#;

    let responses = handle_iq(
        query,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &authenticated_session,
        &ready_phase(&authenticated_jid),
    )
    .await;
    let response = responses
        .first()
        .expect("spaces node pubsub items response");
    assert!(
        response.contains("type='error'")
            && response.contains("not-allowed")
            && response.contains("closed-node"),
        "unauthorized Spaces items read should be closed-node before backfill: {response}"
    );

    let stored = state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&spaces_jid, "private-team", None, &[])
        .await
        .expect("stored private space bookmarks");
    assert!(
        stored.is_empty(),
        "unauthorized Spaces items read must not backfill bookmarks"
    );
}

#[tokio::test]
async fn handle_iq_pubsub_configure_spaces_node_rejects_unsupported_access_models() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "owner").await;
    let owner_jid: BareJid = "owner@example.com".parse().expect("owner jid");
    let bound_jid: FullJid = "owner@example.com/web".parse().expect("bound jid");
    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");

    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "team")
        .await
        .expect("space node");
    state
        .deps
        .protocol
        .pubsub_storage
        .update_node_config(
            &spaces_jid,
            "team",
            &waddle_xmpp::pubsub::NodeConfig::spaces_public(),
        )
        .await
        .expect("space node config");
    state
        .deps
        .protocol
        .pubsub_storage
        .set_affiliation(
            &spaces_jid,
            "team",
            &owner_jid,
            waddle_xmpp::pubsub::Affiliation::Owner,
        )
        .await
        .expect("owner affiliation");

    for access_model in ["presence", "roster", "authorize"] {
        let frame = format!(
            r#"<iq xmlns="jabber:client" id="spaces-config-{access_model}" type="set" to="spaces.example.com">
                <pubsub xmlns="http://jabber.org/protocol/pubsub#owner">
                    <configure node="team">
                        <x xmlns="jabber:x:data" type="submit">
                            <field var="FORM_TYPE" type="hidden"><value>http://jabber.org/protocol/pubsub#node_config</value></field>
                            <field var="pubsub#access_model"><value>{access_model}</value></field>
                        </x>
                    </configure>
                </pubsub>
            </iq>"#
        );

        let responses = handle_iq(
            &frame,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(session.clone()),
            &ready_phase(&bound_jid),
        )
        .await;
        let response = responses.first().expect("configure response");
        assert!(
            response.contains("type='error'") && response.contains("bad-request"),
            "Spaces configure should reject unsupported access_model={access_model}: {response}"
        );
        let stored = state
            .deps
            .protocol
            .pubsub_storage
            .get_node(&spaces_jid, "team")
            .await
            .expect("node lookup")
            .expect("space node");
        assert_eq!(
            stored.config.access_model,
            waddle_xmpp::pubsub::AccessModel::Open,
            "rejected Spaces configure must not mutate the stored node config"
        );
    }
}

#[tokio::test]
async fn handle_iq_pubsub_configure_spaces_node_keeps_spaces_defaults() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "owner").await;
    let owner_jid: BareJid = "owner@example.com".parse().expect("owner jid");
    let bound_jid: FullJid = "owner@example.com/web".parse().expect("bound jid");
    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");

    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "team")
        .await
        .expect("space node");
    state
        .deps
        .protocol
        .pubsub_storage
        .set_affiliation(
            &spaces_jid,
            "team",
            &owner_jid,
            waddle_xmpp::pubsub::Affiliation::Owner,
        )
        .await
        .expect("owner affiliation");

    for (access_model, expected_config) in [
        (
            "whitelist",
            waddle_xmpp::pubsub::NodeConfig::spaces_private(),
        ),
        ("open", waddle_xmpp::pubsub::NodeConfig::spaces_public()),
    ] {
        let frame = format!(
            r#"<iq xmlns="jabber:client" id="spaces-config-ok-{access_model}" type="set" to="spaces.example.com">
                <pubsub xmlns="http://jabber.org/protocol/pubsub#owner">
                    <configure node="team">
                        <x xmlns="jabber:x:data" type="submit">
                            <field var="FORM_TYPE" type="hidden"><value>http://jabber.org/protocol/pubsub#node_config</value></field>
                            <field var="pubsub#access_model"><value>{access_model}</value></field>
                        </x>
                    </configure>
                </pubsub>
            </iq>"#
        );

        let responses = handle_iq(
            &frame,
            "example.com",
            "muc.example.com",
            state.as_ref(),
            &Some(session.clone()),
            &ready_phase(&bound_jid),
        )
        .await;
        let response = responses.first().expect("configure response");
        assert!(
            response.contains("type='result'") || response.contains("type=\"result\""),
            "Spaces configure should accept supported access_model={access_model}: {response}"
        );
        let stored = state
            .deps
            .protocol
            .pubsub_storage
            .get_node(&spaces_jid, "team")
            .await
            .expect("node lookup")
            .expect("space node");
        assert_eq!(
            stored.config, expected_config,
            "supported Spaces configure should use the canonical Spaces defaults"
        );
    }
}

#[tokio::test]
async fn handle_iq_pubsub_configure_spaces_node_merges_partial_form_with_existing_config() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "owner").await;
    let owner_jid: BareJid = "owner@example.com".parse().expect("owner jid");
    let bound_jid: FullJid = "owner@example.com/web".parse().expect("bound jid");
    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");

    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "team")
        .await
        .expect("space node");
    state
        .deps
        .protocol
        .pubsub_storage
        .update_node_config(
            &spaces_jid,
            "team",
            &waddle_xmpp::pubsub::NodeConfig::spaces_public(),
        )
        .await
        .expect("space node config");
    state
        .deps
        .protocol
        .pubsub_storage
        .set_affiliation(
            &spaces_jid,
            "team",
            &owner_jid,
            waddle_xmpp::pubsub::Affiliation::Owner,
        )
        .await
        .expect("owner affiliation");

    let frame = r#"<iq xmlns="jabber:client" id="spaces-config-partial" type="set" to="spaces.example.com">
        <pubsub xmlns="http://jabber.org/protocol/pubsub#owner">
            <configure node="team">
                <x xmlns="jabber:x:data" type="submit">
                    <field var="FORM_TYPE" type="hidden"><value>http://jabber.org/protocol/pubsub#node_config</value></field>
                    <field var="pubsub#max_items"><value>200</value></field>
                </x>
            </configure>
        </pubsub>
    </iq>"#;

    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&bound_jid),
    )
    .await;
    let response = responses.first().expect("configure response");
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Spaces configure should accept partial max_items-only form: {response}"
    );
    let stored = state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&spaces_jid, "team")
        .await
        .expect("node lookup")
        .expect("space node");
    assert_eq!(
        stored.config.access_model,
        waddle_xmpp::pubsub::AccessModel::Open,
        "partial Spaces configure must not synthesize access_model=presence"
    );
    assert_eq!(stored.config.max_items, 200);
    assert_eq!(
        stored.config.publish_model,
        waddle_xmpp::pubsub::PublishModel::Publishers,
        "partial Spaces configure must preserve publisher-gated writes"
    );
}

#[tokio::test]
async fn admin_channels_delete_retracts_duplicate_space_bookmarks() {
    let state = create_test_websocket_state().await;
    crate::admin::channels::register(
        &state.deps.protocol.command_registry,
        std::sync::Arc::clone(&state.deps.app_state),
        std::sync::Arc::clone(&state.deps.protocol.connection_registry),
        state.deps.protocol.user_registry.clone(),
        std::sync::Arc::clone(&state.deps.protocol.sm_session_registry),
        None,
    )
    .await;

    let owner_jid: BareJid = "owner@example.com".parse().expect("owner jid");
    state
        .deps
        .app_state
        .pubsub_storage
        .get_or_create_node(&state.deps.app_state.spaces_jid, "alpha")
        .await
        .expect("alpha space node");
    state
        .deps
        .app_state
        .pubsub_storage
        .get_or_create_node(&state.deps.app_state.spaces_jid, "beta")
        .await
        .expect("beta space node");
    state
        .deps
        .app_state
        .pubsub_storage
        .set_affiliation(
            &state.deps.app_state.spaces_jid,
            "alpha",
            &owner_jid,
            waddle_xmpp::pubsub::Affiliation::Owner,
        )
        .await
        .expect("owner affiliation");

    let room_jid: BareJid = format!("duplicate@{}", state.deps.app_state.muc_domain)
        .parse()
        .expect("room jid");
    state
        .deps
        .app_state
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "test".to_string(),
            channel_id: "duplicate".to_string(),
            config: waddle_xmpp::muc::RoomConfig {
                name: "Duplicated".to_string(),
                persistent: true,
                members_only: false,
                ..waddle_xmpp::muc::RoomConfig::default()
            },
        })
        .await
        .expect("create room");
    state
        .deps
        .app_state
        .channel_space_link_store
        .set(&crate::channel_space_links::ChannelSpaceLink {
            channel_jid: room_jid.clone(),
            space_jid: format!("alpha@{}", state.deps.app_state.spaces_jid.domain())
                .parse()
                .expect("space jid"),
            space_node: crate::space_identity::SpaceNode::from("alpha"),
            created_at: 0,
        })
        .await
        .expect("link channel");

    let item = waddle_xmpp::xep::build_channel_item(
        &waddle_xmpp::ChannelInfo {
            id: "duplicate".to_string(),
            name: "Duplicated".to_string(),
            channel_type: "text".to_string(),
        },
        &state.deps.app_state.muc_domain.to_string(),
    )
    .expect("bookmark item");
    for node in ["alpha", "beta"] {
        state
            .deps
            .app_state
            .pubsub_storage
            .publish_item(&state.deps.app_state.spaces_jid, node, &item, None, false)
            .await
            .expect("publish duplicate bookmark");
    }

    let session = create_test_session(state.as_ref(), "owner").await;
    let bound_jid: FullJid = "owner@example.com/web".parse().expect("bound jid");
    let frame = format!(
        r#"<iq xmlns="jabber:client" id="admin-delete-duplicate" type="set" to="example.com"><command xmlns="http://jabber.org/protocol/commands" node="{}" action="execute"><x xmlns="jabber:x:data" type="submit"><field var="FORM_TYPE" type="hidden"><value>{}</value></field><field var="channel_jid" type="text-single"><value>{}</value></field><field var="confirm" type="text-single"><value>yes</value></field></x></command></iq>"#,
        crate::admin::channels::NODE_DELETE,
        crate::admin::channels::NODE_DELETE,
        room_jid
    );
    let responses = handle_iq(
        &frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&bound_jid),
    )
    .await;
    let response = responses.first().expect("delete command response");
    assert!(
        response.contains("status='completed'") || response.contains("status=\"completed\""),
        "expected completed delete command response: {response}"
    );

    let remaining_nodes = state
        .deps
        .app_state
        .pubsub_storage
        .list_node_names_for_item(&state.deps.app_state.spaces_jid, &room_jid.to_string())
        .await
        .expect("remaining bookmark nodes");
    assert!(
        remaining_nodes.is_empty(),
        "channels:delete should retract duplicate bookmarks from every space node, got: {remaining_nodes:?}"
    );
}

#[tokio::test]
async fn admin_channels_update_retracts_duplicate_space_bookmarks() {
    let state = create_test_websocket_state().await;
    crate::admin::channels::register(
        &state.deps.protocol.command_registry,
        std::sync::Arc::clone(&state.deps.app_state),
        std::sync::Arc::clone(&state.deps.protocol.connection_registry),
        state.deps.protocol.user_registry.clone(),
        std::sync::Arc::clone(&state.deps.protocol.sm_session_registry),
        None,
    )
    .await;

    let owner_jid: BareJid = "owner@example.com".parse().expect("owner jid");
    for node in ["alpha", "beta"] {
        state
            .deps
            .app_state
            .pubsub_storage
            .get_or_create_node(&state.deps.app_state.spaces_jid, node)
            .await
            .expect("space node");
    }
    state
        .deps
        .app_state
        .pubsub_storage
        .set_affiliation(
            &state.deps.app_state.spaces_jid,
            "alpha",
            &owner_jid,
            waddle_xmpp::pubsub::Affiliation::Owner,
        )
        .await
        .expect("owner affiliation");

    let room_jid: BareJid = format!("rename-duplicate@{}", state.deps.app_state.muc_domain)
        .parse()
        .expect("room jid");
    state
        .deps
        .app_state
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "test".to_string(),
            channel_id: "rename-duplicate".to_string(),
            config: waddle_xmpp::muc::RoomConfig {
                name: "Old Name".to_string(),
                persistent: true,
                members_only: false,
                ..waddle_xmpp::muc::RoomConfig::default()
            },
        })
        .await
        .expect("create room");
    state
        .deps
        .app_state
        .channel_space_link_store
        .set(&crate::channel_space_links::ChannelSpaceLink {
            channel_jid: room_jid.clone(),
            space_jid: format!("alpha@{}", state.deps.app_state.spaces_jid.domain())
                .parse()
                .expect("space jid"),
            space_node: crate::space_identity::SpaceNode::from("alpha"),
            created_at: 0,
        })
        .await
        .expect("link channel");
    let old_item = waddle_xmpp::xep::build_channel_item(
        &waddle_xmpp::ChannelInfo {
            id: "rename-duplicate".to_string(),
            name: "Old Name".to_string(),
            channel_type: "text".to_string(),
        },
        &state.deps.app_state.muc_domain.to_string(),
    )
    .expect("old bookmark item");
    for node in ["alpha", "beta"] {
        state
            .deps
            .app_state
            .pubsub_storage
            .publish_item(
                &state.deps.app_state.spaces_jid,
                node,
                &old_item,
                None,
                false,
            )
            .await
            .expect("publish duplicate bookmark");
    }

    let session = create_test_session(state.as_ref(), "owner").await;
    let bound_jid: FullJid = "owner@example.com/web".parse().expect("bound jid");
    let frame = format!(
        r#"<iq xmlns="jabber:client" id="admin-update-duplicate" type="set" to="example.com"><command xmlns="http://jabber.org/protocol/commands" node="{}" action="execute"><x xmlns="jabber:x:data" type="submit"><field var="FORM_TYPE" type="hidden"><value>{}</value></field><field var="channel_jid" type="text-single"><value>{}</value></field><field var="name" type="text-single"><value>New Name</value></field></x></command></iq>"#,
        crate::admin::channels::NODE_UPDATE,
        crate::admin::channels::NODE_UPDATE,
        room_jid
    );
    let responses = handle_iq(
        &frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&bound_jid),
    )
    .await;
    let response = responses.first().expect("update command response");
    assert!(
        response.contains("status='completed'") || response.contains("status=\"completed\""),
        "expected completed update command response: {response}"
    );

    let alpha_items = state
        .deps
        .app_state
        .pubsub_storage
        .get_items(&state.deps.app_state.spaces_jid, "alpha", None, &[])
        .await
        .expect("alpha items");
    assert!(
        alpha_items
            .iter()
            .any(|item| item.id == room_jid.to_string()
                && item
                    .payload_xml
                    .as_ref()
                    .is_some_and(|payload| payload.contains("New Name"))),
        "linked space should retain updated bookmark with new name"
    );
    let bookmark_nodes: std::collections::BTreeSet<_> = state
        .deps
        .app_state
        .pubsub_storage
        .list_node_names_for_item(&state.deps.app_state.spaces_jid, &room_jid.to_string())
        .await
        .expect("bookmark nodes after update")
        .into_iter()
        .collect();
    assert_eq!(
        bookmark_nodes,
        std::collections::BTreeSet::from(["alpha".to_string()]),
        "admin update should retract duplicate bookmarks from stale nodes"
    );
}

#[tokio::test]
async fn admin_channels_create_space_bookmark_grants_space_members_channel_view() {
    let state = create_test_websocket_state().await;
    crate::admin::channels::register(
        &state.deps.protocol.command_registry,
        std::sync::Arc::clone(&state.deps.app_state),
        std::sync::Arc::clone(&state.deps.protocol.connection_registry),
        state.deps.protocol.user_registry.clone(),
        std::sync::Arc::clone(&state.deps.protocol.sm_session_registry),
        None,
    )
    .await;

    let spaces_jid = state.deps.app_state.spaces_jid.clone();
    state
        .deps
        .app_state
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "alpha")
        .await
        .expect("space node");
    let owner_jid: BareJid = "owner@example.com".parse().expect("owner jid");
    state
        .deps
        .app_state
        .pubsub_storage
        .set_affiliation(
            &spaces_jid,
            "alpha",
            &owner_jid,
            waddle_xmpp::pubsub::Affiliation::Owner,
        )
        .await
        .expect("owner affiliation");

    let viewer = create_test_session(state.as_ref(), "viewer").await;
    grant_space_member_for_test(state.as_ref(), "alpha", &viewer.user_jid).await;

    let session = create_test_server_owner_session(state.as_ref(), "owner").await;
    let bound_jid: FullJid = "owner@example.com/web".parse().expect("bound jid");
    let space_jid = format!("alpha@{}", spaces_jid.domain());
    let frame = format!(
        r#"<iq xmlns="jabber:client" id="admin-create-parent" type="set" to="example.com"><command xmlns="http://jabber.org/protocol/commands" node="{}" action="execute"><x xmlns="jabber:x:data" type="submit"><field var="FORM_TYPE" type="hidden"><value>{}</value></field><field var="name" type="text-single"><value>Alpha Parent</value></field><field var="space_jid" type="text-single"><value>{}</value></field></x></command></iq>"#,
        crate::admin::channels::NODE_CREATE,
        crate::admin::channels::NODE_CREATE,
        space_jid
    );
    let responses = handle_iq(
        &frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&bound_jid),
    )
    .await;
    let response = responses.first().expect("create command response");
    assert!(
        response.contains("status='completed'") || response.contains("status=\"completed\""),
        "expected completed create command response: {response}"
    );
    let channel_jid = data_form_value_for_test(response, "channel_jid").expect("channel_jid value");
    let channel_jid: BareJid = channel_jid.parse().expect("channel jid");
    let channel_id = waddle_xmpp::parse_managed_room_jid(&channel_jid).expect("managed channel");

    assert!(
        channel_view_allowed_for_test(state.as_ref(), &channel_id, &viewer.user_jid).await,
        "admin-created XEP-0503 bookmark should write the channel parent tuple"
    );
}

#[tokio::test]
async fn admin_spaces_delete_clears_channel_parent_tuple() {
    let state = create_test_websocket_state().await;
    crate::admin::spaces::register(
        &state.deps.protocol.command_registry,
        std::sync::Arc::clone(&state.deps.app_state),
    )
    .await;

    let spaces_jid = state.deps.app_state.spaces_jid.clone();
    state
        .deps
        .app_state
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "alpha")
        .await
        .expect("space node");
    let owner_jid: BareJid = "owner@example.com".parse().expect("owner jid");
    state
        .deps
        .app_state
        .pubsub_storage
        .set_affiliation(
            &spaces_jid,
            "alpha",
            &owner_jid,
            waddle_xmpp::pubsub::Affiliation::Owner,
        )
        .await
        .expect("owner affiliation");

    let room_jid: BareJid = format!("delete-parent@{}", state.deps.app_state.muc_domain)
        .parse()
        .expect("room jid");
    state
        .deps
        .app_state
        .room_registry
        .ask(waddle_xmpp::muc::room_registry_actor::CreateRoom {
            room_jid: room_jid.clone(),
            waddle_id: "test".to_string(),
            channel_id: "delete-parent".to_string(),
            config: waddle_xmpp::muc::RoomConfig {
                name: "Delete Parent".to_string(),
                persistent: true,
                members_only: false,
                ..waddle_xmpp::muc::RoomConfig::default()
            },
        })
        .await
        .expect("create room");
    state
        .deps
        .app_state
        .channel_space_link_store
        .set(&crate::channel_space_links::ChannelSpaceLink {
            channel_jid: room_jid.clone(),
            space_jid: format!("alpha@{}", spaces_jid.domain())
                .parse()
                .expect("space jid"),
            space_node: crate::space_identity::SpaceNode::from("alpha"),
            created_at: 0,
        })
        .await
        .expect("link channel");
    let item = waddle_xmpp::xep::build_channel_item(
        &waddle_xmpp::ChannelInfo {
            id: "delete-parent".to_string(),
            name: "Delete Parent".to_string(),
            channel_type: "text".to_string(),
        },
        &state.deps.app_state.muc_domain.to_string(),
    )
    .expect("bookmark item");
    state
        .deps
        .app_state
        .pubsub_storage
        .publish_item(&spaces_jid, "alpha", &item, None, false)
        .await
        .expect("publish bookmark");

    let viewer = create_test_session(state.as_ref(), "viewer").await;
    grant_space_member_for_test(state.as_ref(), "alpha", &viewer.user_jid).await;
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Channel, "delete-parent"),
                Relation::new("parent"),
                Subject::userset(SubjectType::Space, "alpha", ""),
            ),
        })
        .await
        .expect("channel parent tuple");
    assert!(
        channel_view_allowed_for_test(state.as_ref(), "delete-parent", &viewer.user_jid).await,
        "test setup should grant channel view through the space parent tuple"
    );

    let session = create_test_server_owner_session(state.as_ref(), "owner").await;
    let bound_jid: FullJid = "owner@example.com/web".parse().expect("bound jid");
    let space_jid = format!("alpha@{}", spaces_jid.domain());
    let frame = format!(
        r#"<iq xmlns="jabber:client" id="admin-delete-space-parent" type="set" to="example.com"><command xmlns="http://jabber.org/protocol/commands" node="{}" action="execute"><x xmlns="jabber:x:data" type="submit"><field var="FORM_TYPE" type="hidden"><value>{}</value></field><field var="space_jid" type="text-single"><value>{}</value></field><field var="confirm" type="text-single"><value>yes</value></field></x></command></iq>"#,
        crate::admin::spaces::NODE_DELETE,
        crate::admin::spaces::NODE_DELETE,
        space_jid
    );
    let responses = handle_iq(
        &frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&bound_jid),
    )
    .await;
    let response = responses.first().expect("delete command response");
    assert!(
        response.contains("status='completed'") || response.contains("status=\"completed\""),
        "expected completed space delete command response: {response}"
    );
    assert!(
        !channel_view_allowed_for_test(state.as_ref(), "delete-parent", &viewer.user_jid).await,
        "spaces:delete should remove the channel parent tuple"
    );
}

#[tokio::test]
async fn spaces_publish_and_retract_sync_channel_space_link_projection() {
    let state = create_test_websocket_state().await;
    let conn = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("db connection");
    conn.execute(
        "INSERT INTO channels (id, name, description, channel_type, position, is_default) VALUES (?, ?, ?, 'text', 0, 0)",
        crate::db_params!["linked", "Linked", "Linked channel description"],
    )
    .await
    .expect("insert channel");
    drop(conn);

    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "team")
        .await
        .expect("space node");

    let session = create_test_server_owner_session(state.as_ref(), "owner").await;
    let bound_jid: FullJid = "owner@example.com/web".parse().expect("bound jid");
    let room_jid: BareJid = "linked@muc.example.com".parse().expect("room jid");

    let publish = r#"<iq xmlns="jabber:client" id="spaces-link-publish" type="set" to="spaces.example.com">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="team">
                <item id="linked@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1" name="Linked" />
                </item>
            </publish>
        </pubsub>
    </iq>"#;
    let publish_responses = handle_iq(
        publish,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session.clone()),
        &ready_phase(&bound_jid),
    )
    .await;
    let publish_response = publish_responses.first().expect("publish response");
    assert!(
        publish_response.contains("type='result'") || publish_response.contains("type=\"result\""),
        "spaces publish should succeed: {publish_response}"
    );
    let link = state
        .deps
        .app_state
        .channel_space_link_store
        .get(&room_jid)
        .await
        .expect("channel-space link")
        .expect("link after spaces publish");
    assert_eq!(link.space_jid.to_string(), "team@spaces.example.com");

    let retract = r#"<iq xmlns="jabber:client" id="spaces-link-retract" type="set" to="spaces.example.com">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <retract node="team">
                <item id="linked@muc.example.com" />
            </retract>
        </pubsub>
    </iq>"#;
    let retract_responses = handle_iq(
        retract,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&bound_jid),
    )
    .await;
    let retract_response = retract_responses.first().expect("retract response");
    assert!(
        retract_response.contains("type='result'") || retract_response.contains("type=\"result\""),
        "spaces retract should succeed: {retract_response}"
    );
    assert!(
        state
            .deps
            .app_state
            .channel_space_link_store
            .get(&room_jid)
            .await
            .expect("channel-space link after retract")
            .is_none(),
        "spaces retract should clear matching channel-space link projection"
    );
}

#[tokio::test]
async fn spaces_publish_accepts_escaped_space_node_and_syncs_link_projection() {
    let state = create_test_websocket_state().await;
    let conn = state
        .deps
        .app_state
        .db_pool
        .global()
        .guard()
        .await
        .expect("db connection");
    conn.execute(
        "INSERT INTO channels (id, name, description, channel_type, position, is_default) VALUES (?, ?, ?, 'text', 0, 0)",
        crate::db_params!["hierarchical", "Hierarchical", "Hierarchical channel description"],
    )
    .await
    .expect("insert channel");
    drop(conn);

    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "music/A")
        .await
        .expect("space node");

    let viewer = create_test_session(state.as_ref(), "viewer").await;
    grant_space_member_for_test(state.as_ref(), "music/A", &viewer.user_jid).await;

    let session = create_test_server_owner_session(state.as_ref(), "owner").await;
    let bound_jid: FullJid = "owner@example.com/web".parse().expect("bound jid");
    let room_jid: BareJid = "hierarchical@muc.example.com".parse().expect("room jid");

    let publish = r#"<iq xmlns="jabber:client" id="spaces-hier-publish" type="set" to="spaces.example.com">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <publish node="music/A">
                <item id="hierarchical@muc.example.com">
                    <conference xmlns="urn:xmpp:bookmarks:1" name="Hierarchical" />
                </item>
            </publish>
        </pubsub>
    </iq>"#;
    let publish_responses = handle_iq(
        publish,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session.clone()),
        &ready_phase(&bound_jid),
    )
    .await;
    let publish_response = publish_responses.first().expect("publish response");
    assert!(
        publish_response.contains("type='result'") || publish_response.contains("type=\"result\""),
        "spaces publish should accept non-JID node ids: {publish_response}"
    );
    let link = state
        .deps
        .app_state
        .channel_space_link_store
        .get(&room_jid)
        .await
        .expect("channel-space link")
        .expect("escaped Space node should sync durable channel-space link");
    assert_eq!(link.space_node, "music/A");
    assert!(
        channel_view_allowed_for_test(state.as_ref(), "hierarchical", &viewer.user_jid).await,
        "publish should still write the channel parent tuple for non-JID Space nodes"
    );

    let retract = r#"<iq xmlns="jabber:client" id="spaces-hier-retract" type="set" to="spaces.example.com">
        <pubsub xmlns="http://jabber.org/protocol/pubsub">
            <retract node="music/A">
                <item id="hierarchical@muc.example.com" />
            </retract>
        </pubsub>
    </iq>"#;
    let retract_responses = handle_iq(
        retract,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&bound_jid),
    )
    .await;
    let retract_response = retract_responses.first().expect("retract response");
    assert!(
        retract_response.contains("type='result'") || retract_response.contains("type=\"result\""),
        "spaces retract should accept non-JID node ids: {retract_response}"
    );
    assert!(
        !channel_view_allowed_for_test(state.as_ref(), "hierarchical", &viewer.user_jid).await,
        "retract should clear the channel parent tuple for non-JID Space nodes"
    );
    assert!(
        state
            .deps
            .app_state
            .channel_space_link_store
            .get(&room_jid)
            .await
            .expect("channel-space link after retract")
            .is_none(),
        "retract should clear the matching exact-node channel-space link"
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
    state
        .deps
        .protocol
        .pubsub_storage
        .update_node_config(
            &spaces_jid,
            "team",
            &waddle_xmpp::pubsub::NodeConfig::spaces_public(),
        )
        .await
        .expect("space node config");

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
async fn handle_iq_disco_info_spaces_node_reports_whitelist_for_private_space() {
    let state = create_test_websocket_state().await;
    let viewer = create_test_session(state.as_ref(), "viewer").await;
    let spaces_jid: BareJid = "spaces.example.com".parse().expect("spaces jid");
    state
        .deps
        .protocol
        .pubsub_storage
        .get_or_create_node(&spaces_jid, "private-team")
        .await
        .expect("space node");
    state
        .deps
        .protocol
        .pubsub_storage
        .update_node_config(
            &spaces_jid,
            "private-team",
            &waddle_xmpp::pubsub::NodeConfig::spaces_private(),
        )
        .await
        .expect("private space node config");

    let viewer_phase = authenticated_phase_for_session(&viewer, "example.com");
    let query = disco_info_iq_frame(
        "space-node-info-private",
        "spaces.example.com",
        Some("private-team"),
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
    let response = responses.first().expect("spaces node disco info response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "expected successful private node disco#info response: {response}"
    );
    assert!(
        response.contains("pubsub#access_model"),
        "expected access model metadata in private node disco#info: {response}"
    );
    assert!(
        response.contains(">whitelist<"),
        "expected private access model=whitelist in metadata: {response}"
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
async fn upload_slot_too_large_error_is_typed_xep0363_iq() {
    let state = create_test_websocket_state().await;
    let session = create_test_session(state.as_ref(), "alice").await;
    let bound_jid: FullJid = "alice@example.com/web".parse().expect("bound jid");
    let ready = ready_phase(&bound_jid);
    let max_size = crate::server::routes::uploads::max_upload_size();
    let too_large = max_size
        .checked_add(1)
        .expect("test max upload size leaves room for an over-limit request");
    let request = Element::builder("request", "urn:xmpp:http:upload:0")
        .attr(
            minidom::rxml::xml_ncname!("filename").to_owned(),
            "huge.bin",
        )
        .attr(
            minidom::rxml::xml_ncname!("size").to_owned(),
            too_large.to_string(),
        )
        .attr(
            minidom::rxml::xml_ncname!("content-type").to_owned(),
            "application/octet-stream",
        )
        .build();
    let frame = stanza_to_xml(&Stanza::Iq(Box::new(Iq::Get {
        from: Some(bound_jid.clone().into()),
        to: Some("upload.example.com".parse().expect("upload jid")),
        id: "upload-too-big-1".to_string(),
        payload: request,
    })));

    let responses = handle_iq(
        &frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready,
    )
    .await;

    assert_eq!(responses.len(), 1, "expected one response: {responses:?}");
    let iq = parse_iq_for_test(&responses[0]);
    let Iq::Error {
        from,
        to,
        id,
        error,
        payload,
    } = iq
    else {
        panic!("expected upload too-large IQ error: {}", responses[0]);
    };

    assert_eq!(id, "upload-too-big-1");
    assert_eq!(
        from.as_ref().map(ToString::to_string).as_deref(),
        Some("upload.example.com")
    );
    assert_eq!(
        to.as_ref().map(ToString::to_string).as_deref(),
        Some("alice@example.com/web")
    );
    assert_eq!(error.type_, xmpp_parsers::stanza_error::ErrorType::Modify);
    assert_eq!(
        error.defined_condition,
        xmpp_parsers::stanza_error::DefinedCondition::NotAcceptable
    );

    let original_request = payload.expect("original upload request payload");
    assert_eq!(original_request.name(), "request");
    assert_eq!(original_request.ns(), "urn:xmpp:http:upload:0");
    assert_eq!(original_request.attr("filename"), Some("huge.bin"));
    assert_eq!(
        original_request.attr("size"),
        Some(too_large.to_string().as_str())
    );

    let app_error = error.other.expect("file-too-large app error");
    assert_eq!(app_error.name(), "file-too-large");
    assert_eq!(app_error.ns(), "urn:xmpp:http:upload:0");
    assert_eq!(
        app_error
            .get_child("max-file-size", "urn:xmpp:http:upload:0")
            .expect("max-file-size")
            .text(),
        max_size.to_string()
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
                rich_payload_opt_in: false,
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

// ---------------------------------------------------------------
// 1:1 DM calling (XEP-0166 Jingle) regression
// ---------------------------------------------------------------
//
// Bug: starting a call in a DM failed with "server returned a stanza
// error: Cancel: feature-not-implemented". A 1:1 `session-initiate`
// IQ is addressed to the peer's full JID; the Jingle handler mints a
// LiveKit transport and forwards the stanza to the peer
// (`RouteToConnection`), emitting NO synchronous frame back to the
// initiator — the peer's client returns the IQ result. The WebSocket
// IQ handler used to treat the empty sans-I/O result as "no handler
// ran" and fall through to a generic `feature-not-implemented`.
//
// This test drives a real `session-initiate` through `handle_iq` with
// the call handlers registered (production wiring when LiveKit is
// configured) and asserts the initiator gets no error while the peer
// receives the forwarded stanza.
#[tokio::test]
async fn dm_call_session_initiate_forwards_to_peer_not_feature_not_implemented() {
    let state = create_test_websocket_state_with_calls().await;
    let alice: FullJid = "alice@example.com/web".parse().expect("alice jid");
    let bob: FullJid = "bob@example.com/phone".parse().expect("bob jid");

    let (alice_tx, _alice_rx) = tokio::sync::mpsc::channel(8);
    let (bob_tx, mut bob_rx) = tokio::sync::mpsc::channel::<OutboundStanza>(8);
    // ADR-0017 Slice 2: delivery reads the actor tree, so register into both.
    super::register_test_connection(state.as_ref(), &alice, alice_tx).await;
    super::register_test_connection(state.as_ref(), &bob, bob_tx).await;

    // Realistic XEP-0166 1:1 session-initiate carrying an XEP-0167
    // Opus RTP description and the Waddle LiveKit transport request
    // placeholder the server rewrites with an issued token.
    let frame = r#"<iq xmlns='jabber:client' id='dm-call-1' type='set' to='bob@example.com/phone'>
        <jingle xmlns='urn:xmpp:jingle:1' action='session-initiate' sid='dmcall1' initiator='alice@example.com/web'>
            <content creator='initiator' name='audio'>
                <description xmlns='urn:xmpp:jingle:apps:rtp:1' media='audio'>
                    <payload-type id='111' name='opus' clockrate='48000' channels='2'/>
                    <rtcp-mux/>
                </description>
                <transport xmlns='urn:waddle:transports:livekit:0'/>
            </content>
        </jingle>
    </iq>"#;

    let responses = handle_iq(
        frame,
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ready_phase(&alice),
    )
    .await;

    let joined = responses.join("\n");
    assert!(
        !joined.contains("feature-not-implemented"),
        "1:1 Jingle session-initiate must not fall through to \
         feature-not-implemented; initiator got: {joined}"
    );

    // The session-initiate must be forwarded to the peer's connection.
    let forwarded = bob_rx
        .try_recv()
        .expect("peer connection must receive the forwarded session-initiate");
    let xml = stanza_to_xml(&forwarded.stanza);
    assert!(
        xml.contains("urn:xmpp:jingle:1") && xml.contains("session-initiate"),
        "peer must receive the forwarded Jingle session-initiate; got: {xml}"
    );
}

// =========================================================================
// XEP-0313 §5.1 MUC archive access gate (#1093)
// =========================================================================

fn mam_room_query_frame(id: &str, room_jid: &str) -> String {
    let query =
        xmpp_parsers::minidom::Element::builder("query", waddle_xmpp_core::mam::MAM_NS).build();
    iq_set_frame(id, room_jid, query)
}

async fn upsert_test_channel(state: &WebSocketState, id: &str, members_only: bool) {
    crate::server::xmpp_state::upsert_xmpp_channel(
        state.deps.app_state.db_pool.global_actor().clone(),
        &crate::server::xmpp_state::XmppChannelUpsert {
            id: id.to_string(),
            name: id.to_string(),
            description: None,
            channel_type: "channel".to_string(),
            position: 0,
            is_default: false,
            pin_permission: waddle_xmpp::muc::PinPermission::Anyone,
            members_only,
            public_room: !members_only,
        },
    )
    .await
    .expect("channel upsert");
}

#[tokio::test]
async fn mam_members_only_channel_query_returns_forbidden_for_non_member() {
    let state = create_test_websocket_state().await;
    upsert_test_channel(state.as_ref(), "secret-ops", true).await;

    let mallory: FullJid = "mallory@example.com/web".parse().expect("jid");
    let session = Session::new("mallory@example.com", "mallory", "mallory");

    let responses = handle_iq(
        &mam_room_query_frame("mam-gate-1", "secret-ops@muc.example.com"),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&mallory),
    )
    .await;

    assert_eq!(
        responses.len(),
        1,
        "non-member MAM query must yield exactly one error frame: {responses:?}"
    );
    assert!(
        responses[0].contains("<forbidden"),
        "non-member MAM query on a members-only channel must be forbidden: {responses:?}"
    );
}

#[tokio::test]
async fn mam_members_only_channel_query_succeeds_for_channel_member() {
    let state = create_test_websocket_state().await;
    upsert_test_channel(state.as_ref(), "secret-ops", true).await;

    let alice: FullJid = "alice@example.com/web".parse().expect("jid");
    let session = Session::new("alice@example.com", "alice", "alice");
    state
        .deps
        .app_state
        .permission_actor
        .ask(WriteTuple {
            tuple: Tuple::new(
                Object::new(ObjectType::Channel, "secret-ops"),
                Relation::new("member"),
                Subject::user(&session.user_jid),
            ),
        })
        .await
        .expect("channel member tuple");

    let responses = handle_iq(
        &mam_room_query_frame("mam-gate-2", "secret-ops@muc.example.com"),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&alice),
    )
    .await;

    let fin = responses.last().expect("member MAM query must respond");
    assert!(
        fin.contains("<fin") && fin.contains("type='result'"),
        "channel member MAM query must return a result fin: {responses:?}"
    );
}

#[tokio::test]
async fn mam_public_channel_query_succeeds_for_non_member() {
    let state = create_test_websocket_state().await;
    upsert_test_channel(state.as_ref(), "town-square", false).await;

    let mallory: FullJid = "mallory@example.com/web".parse().expect("jid");
    let session = Session::new("mallory@example.com", "mallory", "mallory");

    let responses = handle_iq(
        &mam_room_query_frame("mam-gate-3", "town-square@muc.example.com"),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&mallory),
    )
    .await;

    let fin = responses
        .last()
        .expect("public-room MAM query must respond");
    assert!(
        fin.contains("<fin") && fin.contains("type='result'"),
        "public channel MAM query must stay open to non-members: {responses:?}"
    );
}

/// Spawn an unmanaged members-only room actor with `member_jid` given
/// Member affiliation via the same join message the presence path uses.
async fn create_members_only_room_with_member(
    state: &WebSocketState,
    room_jid: &BareJid,
    member_jid: &FullJid,
) {
    let config = waddle_xmpp::muc::RoomConfig {
        name: "war-room".to_string(),
        members_only: true,
        ..Default::default()
    };
    let actor = get_or_create_room_actor(
        state,
        room_jid,
        config,
        "default".to_string(),
        "default".to_string(),
    )
    .await
    .expect("room actor")
    .actor_ref;
    actor
        .ask(waddle_xmpp::muc::room_actor::JoinWithAffiliation {
            sender_jid: member_jid.clone(),
            nick: "member".to_string(),
            affiliation_grant: JoinAffiliationGrant::Resolver(Affiliation::Member),
            local_domain: "example.com".to_string(),
            admission_revision: 0,
        })
        .await
        .expect("member join");
}

#[tokio::test]
async fn mam_members_only_unmanaged_room_query_returns_forbidden_for_non_member() {
    let state = create_test_websocket_state().await;
    let room_jid: BareJid = "war-room@muc.example.com".parse().expect("room jid");
    let member: FullJid = "alice@example.com/web".parse().expect("jid");
    create_members_only_room_with_member(state.as_ref(), &room_jid, &member).await;

    let mallory: FullJid = "mallory@example.com/web".parse().expect("jid");
    let session = Session::new("mallory@example.com", "mallory", "mallory");

    let responses = handle_iq(
        &mam_room_query_frame("mam-gate-4", "war-room@muc.example.com"),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&mallory),
    )
    .await;

    assert_eq!(
        responses.len(),
        1,
        "expected one error frame: {responses:?}"
    );
    assert!(
        responses[0].contains("<forbidden"),
        "non-member MAM query on a members-only unmanaged room must be forbidden: {responses:?}"
    );
}

#[tokio::test]
async fn mam_members_only_unmanaged_room_query_succeeds_for_member() {
    let state = create_test_websocket_state().await;
    let room_jid: BareJid = "war-room@muc.example.com".parse().expect("room jid");
    let member: FullJid = "alice@example.com/web".parse().expect("jid");
    create_members_only_room_with_member(state.as_ref(), &room_jid, &member).await;

    let session = Session::new("alice@example.com", "alice", "alice");
    let responses = handle_iq(
        &mam_room_query_frame("mam-gate-5", "war-room@muc.example.com"),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&member),
    )
    .await;

    let fin = responses.last().expect("member MAM query must respond");
    assert!(
        fin.contains("<fin") && fin.contains("type='result'"),
        "room member MAM query must return a result fin: {responses:?}"
    );
}

#[tokio::test]
async fn mam_room_query_without_bound_session_returns_forbidden() {
    let state = create_test_websocket_state().await;
    upsert_test_channel(state.as_ref(), "town-square", false).await;

    let responses = handle_iq(
        &mam_room_query_frame("mam-gate-6", "town-square@muc.example.com"),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &None,
        &ConnectionPhase::Unauthenticated,
    )
    .await;

    assert_eq!(
        responses.len(),
        1,
        "expected one error frame: {responses:?}"
    );
    assert!(
        responses[0].contains("<forbidden"),
        "room MAM query without a bound session must be forbidden: {responses:?}"
    );
}

#[tokio::test]
async fn mam_unmanaged_room_without_live_actor_fails_closed() {
    let state = create_test_websocket_state().await;

    let mallory: FullJid = "mallory@example.com/web".parse().expect("jid");
    let session = Session::new("mallory@example.com", "mallory", "mallory");

    let responses = handle_iq(
        &mam_room_query_frame("mam-gate-7", "ghost-room@muc.example.com"),
        "example.com",
        "muc.example.com",
        state.as_ref(),
        &Some(session),
        &ready_phase(&mallory),
    )
    .await;

    assert_eq!(
        responses.len(),
        1,
        "expected one error frame: {responses:?}"
    );
    assert!(
        responses[0].contains("<forbidden"),
        "an unmanaged room with no live actor has no admission data; the \
         archive gate must fail closed: {responses:?}"
    );
}
