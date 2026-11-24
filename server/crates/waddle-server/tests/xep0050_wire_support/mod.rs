//! Shared WebSocket helpers for the XEP-0050 wire suites.

use minidom::Element;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::xep::xep0004::{DataForm, Field, FormType, ToElement};

pub const CLIENT: &str = "jabber:client";
pub const COMMANDS: &str = "http://jabber.org/protocol/commands";
pub const DATA: &str = "jabber:x:data";
pub const INSPECT: &str = "urn:waddle:admin:push:inspect:0";

pub async fn client(server: &TestServer, user: &str, password: &str) -> WsXmppClient {
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

pub fn submit(node: &str, owner: &str, target: &str) -> DataForm {
    DataForm::new(FormType::Submit)
        .add_field(Field::form_type(node))
        .add_field(Field::text_single("owner", owner))
        .add_field(Field::text_single("node", target))
}

/// Send a command IQ and return the response. `attrs` are extra `<command/>`
/// attributes (action, sessionid, status); only `node` is added here.
pub async fn send(
    client: &mut WsXmppClient,
    id: &str,
    to: &str,
    node: &str,
    attrs: &[(&str, &str)],
    form: Option<DataForm>,
    forged_from: bool,
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

pub fn value(iq: &Element, field: &str) -> String {
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

pub fn assert_error(iq: &Element, condition: &str) {
    assert_eq!(iq.attr("type"), Some("error"), "{iq:?}");
    assert!(
        iq.get_child("error", CLIENT)
            .expect("error")
            .get_child(condition, "urn:ietf:params:xml:ns:xmpp-stanzas")
            .is_some(),
        "{iq:?}"
    );
}

/// Register an APNs device through the push-service wizard and return the
/// allocated push node.
pub async fn register_apns(client: &mut WsXmppClient, app: &str) -> String {
    let initial = send(
        client,
        &format!("register-{app}"),
        "push.localhost",
        "register-device",
        &[("action", "execute")],
        None,
        false,
    )
    .await;
    let session = initial
        .get_child("command", COMMANDS)
        .expect("command")
        .attr("sessionid")
        .expect("session")
        .to_string();
    let form = DataForm::new(FormType::Submit)
        .add_field(Field::form_type(
            "urn:waddle:push-service:commands:register-device:0",
        ))
        .add_field(Field::text_single("platform", "apns"))
        .add_field(Field::text_single("environment", "prod"))
        .add_field(Field::text_single("app-id", app))
        .add_field(Field::text_single("apns-token", "ab".repeat(32)));
    let complete = send(
        client,
        &format!("complete-{app}"),
        "push.localhost",
        "register-device",
        &[("action", "complete"), ("sessionid", &session)],
        Some(form),
        false,
    )
    .await;
    assert_eq!(complete.attr("type"), Some("result"), "{complete:?}");
    value(&complete, "node")
}
