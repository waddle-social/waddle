//! XEP-0050 session semantics (§2.4 execute resolution, request status,
//! correctable errors, x:data cancel, xml:lang) over native WebSocket.

use minidom::Element;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::xep::xep0004::{DataForm, Field, FormType, ToElement};

const CLIENT: &str = "jabber:client";
const COMMANDS: &str = "http://jabber.org/protocol/commands";
const DATA: &str = "jabber:x:data";
const INSPECT: &str = "urn:waddle:admin:push:inspect:0";
const RETIRE: &str = "urn:waddle:admin:push:retire:0";

async fn client(server: &TestServer, user: &str, password: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(
        &server.ws_url(),
        "localhost",
        user,
        password,
        "operator-push",
    )
    .await
    .expect("connect")
}

fn submit(node: &str, owner: &str, target: &str) -> DataForm {
    DataForm::new(FormType::Submit)
        .add_field(Field::form_type(node))
        .add_field(Field::text_single("owner", owner))
        .add_field(Field::text_single("node", target))
}

/// Send a command IQ. `attrs` are extra `<command/>` attributes (action,
/// sessionid, status); the helper adds only `node`.
async fn send(
    client: &mut WsXmppClient,
    id: &str,
    to: &str,
    node: &str,
    attrs: &[(&str, &str)],
    form: Option<DataForm>,
) -> Element {
    let mut cmd = Element::builder("command", COMMANDS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    for (name, value) in attrs {
        cmd = cmd.attr(
            minidom::rxml::NcName::try_from(name.to_string()).expect("ncname"),
            *value,
        );
    }
    if let Some(form) = form {
        cmd = cmd.append(form.to_element());
    }
    let iq = Element::builder("iq", CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .append(cmd.build())
        .build();
    let mut bytes = Vec::new();
    iq.write_to(&mut bytes).expect("serialize");
    client
        .send(&String::from_utf8(bytes).expect("utf8"))
        .await
        .expect("send");
    let response = client
        .recv_matching(|frame| frame.contains(id) && frame.contains("<iq"))
        .await
        .expect("response");
    response.parse().expect("response XML")
}

fn command_of(iq: &Element) -> &Element {
    assert_eq!(iq.attr("type"), Some("result"), "{iq:?}");
    iq.get_child("command", COMMANDS).expect("command")
}

fn lang_of(cmd: &Element) -> Option<String> {
    let xml = minidom::rxml::Namespace::XML;
    cmd.attr_ns(&xml, "lang").map(str::to_string)
}

async fn admin_session(
    server: &TestServer,
    admin: &mut WsXmppClient,
    id: &str,
    attrs: &[(&str, &str)],
) -> (String, Element) {
    let _ = server;
    let prompt = send(admin, id, "localhost", INSPECT, attrs, None).await;
    let cmd = command_of(&prompt);
    let session = cmd.attr("sessionid").expect("session").to_string();
    (session, prompt.clone())
}

async fn setup() -> (TestServer, WsXmppClient, String) {
    let password = "alice-push-password";
    let server = TestServer::start_with_extra_accounts(&[("alice", password)]);
    let mut alice =
        WsXmppClient::connect_and_auth(&server.ws_url(), "localhost", "alice", password, "s")
            .await
            .expect("connect");
    let reg = send(
        &mut alice,
        "reg-1",
        "push.localhost",
        "register-device",
        &[("action", "execute")],
        None,
    )
    .await;
    let session = command_of(&reg)
        .attr("sessionid")
        .expect("session")
        .to_string();
    let form = DataForm::new(FormType::Submit)
        .add_field(Field::form_type(
            "urn:waddle:push-service:commands:register-device:0",
        ))
        .add_field(Field::text_single("platform", "apns"))
        .add_field(Field::text_single("environment", "prod"))
        .add_field(Field::text_single("app-id", "xep0050.app"))
        .add_field(Field::text_single("apns-token", "ab".repeat(32)));
    let done = send(
        &mut alice,
        "reg-2",
        "push.localhost",
        "register-device",
        &[("action", "complete"), ("sessionid", &session)],
        Some(form),
    )
    .await;
    let node = command_of(&done)
        .get_child("x", DATA)
        .expect("form")
        .children()
        .find(|c| c.attr("var") == Some("node"))
        .expect("node field")
        .get_child("value", DATA)
        .expect("value")
        .text();
    let admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        "localhost",
        "admin",
        server.fixed_account_password(),
        "a",
    )
    .await
    .expect("admin");
    (server, admin, node)
}

#[tokio::test]
async fn xep0050_requester_status_is_ignored_and_every_response_states_xml_lang() {
    let (server, mut admin, _node) = setup().await;
    // §2.4: requester `status` "MUST" be ignored, including 'execute' (the
    // spec's own start example) and values outside the enumeration.
    for (id, status) in [("st-execute", "execute"), ("st-bogus", "bogus")] {
        let (_, prompt) = admin_session(
            &server,
            &mut admin,
            id,
            &[("action", "execute"), ("status", status)],
        )
        .await;
        let cmd = command_of(&prompt);
        assert_eq!(cmd.attr("status"), Some("executing"));
        assert_eq!(
            lang_of(cmd).as_deref(),
            Some("en"),
            "responder SHOULD state xml:lang"
        );
    }
    // absent action and status together also start the command
    let (_, prompt) = admin_session(&server, &mut admin, "st-none", &[("status", "execute")]).await;
    assert_eq!(command_of(&prompt).attr("status"), Some("executing"));
}

#[tokio::test]
async fn xep0050_execute_and_absent_action_resolve_to_advertised_complete() {
    let (server, mut admin, node) = setup().await;
    for (id, attrs) in [("exec-action", Some("execute")), ("exec-absent", None)] {
        let (session, _) = admin_session(
            &server,
            &mut admin,
            &format!("{id}-1"),
            &[("action", "execute")],
        )
        .await;
        let mut a = vec![("sessionid", session.as_str())];
        if let Some(action) = attrs {
            a.push(("action", action));
        }
        let done = send(
            &mut admin,
            &format!("{id}-2"),
            "localhost",
            INSPECT,
            &a,
            Some(submit(INSPECT, "alice@localhost", &node)),
        )
        .await;
        let cmd = command_of(&done);
        assert_eq!(cmd.attr("status"), Some("completed"), "{done:?}");
        assert_eq!(lang_of(cmd).as_deref(), Some("en"));
    }
}

#[tokio::test]
async fn xep0050_cancel_form_is_submit_and_correctable_error_keeps_session() {
    let (server, mut admin, node) = setup().await;
    let (session, _) = admin_session(&server, &mut admin, "fix-1", &[("action", "execute")]).await;
    // A form missing required fields is correctable: same sessionid works after.
    let bad = send(
        &mut admin,
        "fix-2",
        "localhost",
        INSPECT,
        &[("action", "complete"), ("sessionid", &session)],
        Some(DataForm::new(FormType::Submit).add_field(Field::form_type(INSPECT))),
    )
    .await;
    assert_eq!(bad.attr("type"), Some("error"), "{bad:?}");
    // §2.4: x type='cancel' is considered type='submit'.
    let mut cancel_form = submit(INSPECT, "alice@localhost", &node);
    cancel_form.form_type = FormType::Cancel;
    let done = send(
        &mut admin,
        "fix-3",
        "localhost",
        INSPECT,
        &[("action", "complete"), ("sessionid", &session)],
        Some(cancel_form),
    )
    .await;
    assert_eq!(
        command_of(&done).attr("status"),
        Some("completed"),
        "{done:?}"
    );
}

#[tokio::test]
async fn xep0050_node_on_wrong_service_is_item_not_found() {
    let (_server, mut admin, _node) = setup().await;
    // §4.4: "The responding JID cannot find the requested command node."
    for (id, to, node) in [
        ("wrong-1", "push.localhost", INSPECT),
        ("wrong-2", "localhost", "register-device"),
    ] {
        let response = send(&mut admin, id, to, node, &[("action", "execute")], None).await;
        assert_eq!(response.attr("type"), Some("error"), "{response:?}");
        assert!(response
            .get_child("error", CLIENT)
            .expect("error")
            .get_child("item-not-found", "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some());
    }
}
