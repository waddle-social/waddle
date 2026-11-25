//! XEP-0050 session semantics (§2.4 execute resolution, request status,
//! correctable errors, x:data cancel, xml:lang) over native WebSocket.

mod xep0050_wire_support;

use minidom::Element;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::xep::xep0004::{DataForm, Field, FormType};
use xep0050_wire_support::{assert_error, client, register_apns, send, submit, COMMANDS, INSPECT};

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
    let prompt = send(admin, id, "localhost", INSPECT, attrs, None, false).await;
    let cmd = command_of(&prompt);
    let session = cmd.attr("sessionid").expect("session").to_string();
    (session, prompt.clone())
}

async fn complete(admin: &mut WsXmppClient, id: &str, session: &str, form: DataForm) -> Element {
    send(
        admin,
        id,
        "localhost",
        INSPECT,
        &[("action", "complete"), ("sessionid", session)],
        Some(form),
        false,
    )
    .await
}

async fn setup() -> (TestServer, WsXmppClient, String) {
    let password = "alice-push-password";
    let server = TestServer::start_with_extra_accounts(&[("alice", password)]);
    let mut alice = client(&server, "alice", password).await;
    let node = register_apns(&mut alice, "xep0050.app").await;
    let admin = client(&server, "admin", server.fixed_account_password()).await;
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
            false,
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
    let bad = complete(
        &mut admin,
        "fix-2",
        &session,
        DataForm::new(FormType::Submit).add_field(Field::form_type(INSPECT)),
    )
    .await;
    assert_eq!(bad.attr("type"), Some("error"), "{bad:?}");
    // §2.4: x type='cancel' is considered type='submit'.
    let mut cancel_form = submit(INSPECT, "alice@localhost", &node);
    cancel_form.form_type = FormType::Cancel;
    let done = complete(&mut admin, "fix-3", &session, cancel_form).await;
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
        let response = send(
            &mut admin,
            id,
            to,
            node,
            &[("action", "execute")],
            None,
            false,
        )
        .await;
        assert_error(&response, "item-not-found");
    }
}
