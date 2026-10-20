//! Dedicated XEP-0050/XEP-0004 operator push commands over native WebSocket.

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

async fn command(
    client: &mut WsXmppClient,
    id: &str,
    to: &str,
    node: &str,
    session: Option<(&str, &str)>,
    form: Option<DataForm>,
    forged_from: bool,
) -> Element {
    let mut cmd = Element::builder("command", COMMANDS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node)
        .attr(
            minidom::rxml::xml_ncname!("action").to_owned(),
            session.map(|(_, action)| action).unwrap_or("execute"),
        );
    if let Some((session, _)) = session {
        cmd = cmd.attr(minidom::rxml::xml_ncname!("sessionid").to_owned(), session);
    }
    if let Some(form) = form {
        cmd = cmd.append(form.to_element());
    }
    let mut iq = Element::builder("iq", CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to);
    if forged_from {
        iq = iq.attr(
            minidom::rxml::xml_ncname!("from").to_owned(),
            "admin@localhost/forged",
        );
    }
    let mut bytes = Vec::new();
    iq.append(cmd.build())
        .build()
        .write_to(&mut bytes)
        .expect("serialize");
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

fn value(iq: &Element, field: &str) -> String {
    iq.get_child("command", COMMANDS)
        .expect("command")
        .get_child("x", DATA)
        .expect("form")
        .children()
        .find(|child| child.attr("var") == Some(field))
        .expect("field")
        .get_child("value", DATA)
        .expect("value")
        .text()
}

fn assert_error(iq: &Element, condition: &str) {
    assert_eq!(iq.attr("type"), Some("error"), "{iq:?}");
    assert!(
        iq.get_child("error", CLIENT)
            .expect("error")
            .get_child(condition, "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some(),
        "{iq:?}"
    );
}

async fn register_apns(client: &mut WsXmppClient, app: &str) -> String {
    let id = format!("register-{app}");
    let initial = command(
        client,
        &id,
        "push.localhost",
        "register-device",
        None,
        None,
        false,
    )
    .await;
    let session = initial
        .get_child("command", COMMANDS)
        .expect("command")
        .attr("sessionid")
        .expect("session");
    let form = DataForm::new(FormType::Submit)
        .add_field(Field::form_type(
            "urn:waddle:push-service:commands:register-device:0",
        ))
        .add_field(Field::text_single("platform", "apns"))
        .add_field(Field::text_single("environment", "prod"))
        .add_field(Field::text_single("app-id", app))
        .add_field(Field::text_single("apns-token", "ab".repeat(32)));
    let complete = command(
        client,
        &format!("complete-{app}"),
        "push.localhost",
        "register-device",
        Some((session, "complete")),
        Some(form),
        false,
    )
    .await;
    assert_eq!(complete.attr("type"), Some("result"), "{complete:?}");
    value(&complete, "node")
}

#[tokio::test]
async fn xep0050_operator_push_retirement_requires_confirm_and_is_exact_and_idempotent() {
    let password = "alice-push-password";
    let server = TestServer::start_with_extra_accounts(&[("alice", password)]);
    let mut owner = client(&server, "alice", password).await;
    let node = register_apns(&mut owner, "retire-target").await;
    let sibling = register_apns(&mut owner, "retire-sibling").await;
    let mut admin = client(&server, "admin", server.fixed_account_password()).await;

    let initial = command(
        &mut admin,
        "retire-form",
        "localhost",
        RETIRE,
        None,
        None,
        false,
    )
    .await;
    let cmd = initial.get_child("command", COMMANDS).expect("command");
    assert_eq!(cmd.attr("status"), Some("executing"));
    assert!(cmd
        .get_child("actions", COMMANDS)
        .expect("actions")
        .get_child("complete", COMMANDS)
        .is_some());
    assert_eq!(value(&initial, "confirm"), "0");

    let mut multivalue =
        submit(RETIRE, "alice@localhost", &node).add_field(Field::boolean("confirm", true));
    multivalue
        .fields
        .iter_mut()
        .find(|field| field.var.as_deref() == Some("node"))
        .expect("node field")
        .values
        .push(sibling.clone());
    for (id, form) in [
        ("missing-confirm", submit(RETIRE, "alice@localhost", &node)),
        (
            "false-confirm",
            submit(RETIRE, "alice@localhost", &node).add_field(Field::boolean("confirm", false)),
        ),
        (
            "resource-owner",
            submit(RETIRE, "alice@localhost/resource", &node)
                .add_field(Field::boolean("confirm", true)),
        ),
        (
            "foreign-form",
            submit(INSPECT, "alice@localhost", &node).add_field(Field::boolean("confirm", true)),
        ),
        (
            "duplicate-node",
            submit(RETIRE, "alice@localhost", &node)
                .add_field(Field::text_single("node", &sibling))
                .add_field(Field::boolean("confirm", true)),
        ),
        ("multiple-node-values", multivalue),
    ] {
        let response = command(&mut admin, id, "localhost", RETIRE, None, Some(form), false).await;
        assert_error(&response, "bad-request");
    }
    for (id, action) in [("retire-prev", "prev"), ("retire-next", "next")] {
        let prompt = command(
            &mut admin,
            &format!("{id}-prompt"),
            "localhost",
            RETIRE,
            None,
            None,
            false,
        )
        .await;
        let session = prompt
            .get_child("command", COMMANDS)
            .expect("command")
            .attr("sessionid")
            .expect("session");
        let response = command(
            &mut admin,
            id,
            "localhost",
            RETIRE,
            Some((session, action)),
            Some(
                submit(RETIRE, "alice@localhost", &node).add_field(Field::boolean("confirm", true)),
            ),
            false,
        )
        .await;
        assert_error(&response, "bad-request");
        assert!(response
            .get_child("error", CLIENT)
            .expect("error")
            .get_child("bad-action", COMMANDS)
            .is_some());
    }
    let prompt = command(
        &mut admin,
        "missing-form-prompt",
        "localhost",
        RETIRE,
        None,
        None,
        false,
    )
    .await;
    let session = prompt
        .get_child("command", COMMANDS)
        .expect("command")
        .attr("sessionid")
        .expect("session");
    let missing_form = command(
        &mut admin,
        "missing-complete-form",
        "localhost",
        RETIRE,
        Some((session, "complete")),
        None,
        false,
    )
    .await;
    assert_error(&missing_form, "bad-request");
    let inspected = command(
        &mut admin,
        "before-retire",
        "localhost",
        INSPECT,
        None,
        Some(submit(INSPECT, "alice@localhost", &node)),
        false,
    )
    .await;
    assert_eq!(value(&inspected, "status"), "active");
    assert_eq!(value(&inspected, "provider_capacity"), "10000");
    assert_eq!(value(&inspected, "provider_unresolved_failed"), "0");
    assert_eq!(value(&inspected, "outbox_unresolved_failed"), "0");

    let form = submit(RETIRE, "alice@localhost", &node).add_field(Field::boolean("confirm", true));
    let retired = command(
        &mut admin,
        "retire-confirmed",
        "localhost",
        RETIRE,
        cmd.attr("sessionid").map(|session| (session, "complete")),
        Some(form.clone()),
        false,
    )
    .await;
    assert_eq!(retired.attr("type"), Some("result"), "{retired:?}");
    assert_eq!(value(&retired, "status"), "retired");
    for (id, exact_node, expected) in [
        ("target-disabled", &node, "disabled"),
        ("sibling-active", &sibling, "active"),
    ] {
        let inspected = command(
            &mut admin,
            id,
            "localhost",
            INSPECT,
            None,
            Some(submit(INSPECT, "alice@localhost", exact_node)),
            false,
        )
        .await;
        assert_eq!(value(&inspected, "status"), expected);
        assert_eq!(value(&inspected, "provider_unresolved_failed"), "0");
        assert_eq!(value(&inspected, "outbox_unresolved_failed"), "0");
    }
    let repeated = command(
        &mut admin,
        "retire-repeated",
        "localhost",
        RETIRE,
        None,
        Some(form),
        false,
    )
    .await;
    assert_eq!(repeated.attr("type"), Some("result"));
    let wrong_owner = command(
        &mut admin,
        "wrong-owner",
        "localhost",
        INSPECT,
        None,
        Some(submit(INSPECT, "admin@localhost", &node)),
        false,
    )
    .await;
    assert_eq!(value(&wrong_owner, "status"), "missing");
    assert_eq!(value(&wrong_owner, "provider_retained"), "0");
}

#[tokio::test]
async fn xep0050_operator_push_forbids_non_admin_and_wrong_component() {
    let password = "alice-push-password";
    let server = TestServer::start_with_extra_accounts(&[("alice", password)]);
    let mut alice = client(&server, "alice", password).await;
    let node = register_apns(&mut alice, "protected-target").await;
    for (id, node_name) in [("forbidden-inspect", INSPECT), ("forbidden-retire", RETIRE)] {
        let form =
            submit(node_name, "alice@localhost", &node).add_field(Field::boolean("confirm", true));
        let response = command(
            &mut alice,
            id,
            "localhost",
            node_name,
            None,
            Some(form),
            false,
        )
        .await;
        assert_error(&response, "forbidden");
    }
    let spoofed = command(
        &mut alice,
        "forged-admin",
        "localhost",
        RETIRE,
        None,
        Some(submit(RETIRE, "alice@localhost", &node).add_field(Field::boolean("confirm", true))),
        true,
    )
    .await;
    assert_eq!(spoofed.attr("type"), Some("error"));
    let mut admin = client(&server, "admin", server.fixed_account_password()).await;
    let wrong_component = command(
        &mut admin,
        "wrong-component",
        "push.localhost",
        RETIRE,
        None,
        Some(submit(RETIRE, "alice@localhost", &node).add_field(Field::boolean("confirm", true))),
        false,
    )
    .await;
    assert_error(&wrong_component, "service-unavailable");
    let inspected = command(
        &mut admin,
        "still-active",
        "localhost",
        INSPECT,
        None,
        Some(submit(INSPECT, "alice@localhost", &node)),
        false,
    )
    .await;
    assert_eq!(value(&inspected, "status"), "active");
}
