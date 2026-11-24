//! Dedicated XEP-0050/XEP-0004 operator push commands over native WebSocket.

use minidom::Element;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::xep::xep0004::{DataForm, Field};

mod xep0050_wire_support;

use xep0050_wire_support::{
    assert_error, client, register_apns, send, submit, value, CLIENT, COMMANDS, INSPECT,
};

const RETIRE: &str = "urn:waddle:admin:push:retire:0";

async fn command(
    client: &mut WsXmppClient,
    id: &str,
    to: &str,
    node: &str,
    session: Option<(&str, &str)>,
    form: Option<DataForm>,
    forged_from: bool,
) -> Element {
    let action = session.map(|(_, action)| action).unwrap_or("execute");
    let mut attrs = vec![("action", action)];
    if let Some((session, _)) = session {
        attrs.push(("sessionid", session));
    }
    send(client, id, to, node, &attrs, form, forged_from).await
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
    assert_error(&wrong_component, "item-not-found");
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
