//! Multi-step XEP-0050 composer integration test against a scripted
//! fake Push Service. Pins the EXACT wire shape of each stage so the
//! composer can't silently drift away from XEP-0050 §3.

#![cfg(all(feature = "native", not(target_arch = "wasm32")))]

use std::cell::RefCell;

use minidom::Element;
use waddle_xmpp_client::error::{ClientError, ClientResult, StanzaError, StanzaErrorType};
use waddle_xmpp_client::push::{
    register_push_device, CommandDriver, PushDeviceCredentials, PushEnvironment,
    REGISTER_DEVICE_FORM_TYPE, REGISTER_DEVICE_NODE,
};
use waddle_xmpp_client::xep::xep0050::NS_COMMANDS;

const NS_DATA_FORMS: &str = "jabber:x:data";
const NS_CLIENT: &str = "jabber:client";

struct ScriptedDriver {
    transcript: RefCell<Vec<Element>>,
    responses: RefCell<Vec<ClientResult<Element>>>,
}

impl ScriptedDriver {
    fn new(responses: Vec<ClientResult<Element>>) -> Self {
        Self {
            transcript: RefCell::new(Vec::new()),
            responses: RefCell::new(responses),
        }
    }

    fn transcript(&self) -> Vec<Element> {
        self.transcript.borrow().clone()
    }
}

impl CommandDriver for ScriptedDriver {
    async fn send_iq(&self, iq: Element) -> ClientResult<Element> {
        self.transcript.borrow_mut().push(iq);
        // Remove from the front so the fixture order matches the
        // calling order.
        if self.responses.borrow().is_empty() {
            return Err(protocol_error("scripted driver exhausted"));
        }
        self.responses.borrow_mut().remove(0)
    }
}

fn protocol_error(text: &str) -> ClientError {
    ClientError::StanzaError(StanzaError {
        error_type: StanzaErrorType::Cancel,
        condition: "bad-request".to_string(),
        text: Some(text.to_string()),
    })
}

fn iq_with_command(command: Element) -> Element {
    Element::builder("iq", NS_CLIENT)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(command)
        .build()
}

fn empty_form_request() -> Element {
    Element::builder("x", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "form")
        .append(form_type_field("form"))
        .build()
}

fn form_type_field(form_kind: &str) -> Element {
    let field = Element::builder("field", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "hidden")
        .append(
            Element::builder("value", NS_DATA_FORMS)
                .append(REGISTER_DEVICE_FORM_TYPE)
                .build(),
        )
        .build();
    let _ = form_kind; // form-vs-result share the same FORM_TYPE field shape
    field
}

fn executing_response_with_form(session_id: &str) -> Element {
    let command = Element::builder("command", NS_COMMANDS)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            REGISTER_DEVICE_NODE,
        )
        .attr(
            minidom::rxml::xml_ncname!("sessionid").to_owned(),
            session_id,
        )
        .attr(minidom::rxml::xml_ncname!("status").to_owned(), "executing")
        .append(empty_form_request())
        .build();
    iq_with_command(command)
}

fn completed_response_with_outcome(node_id: &str, device_id: &str) -> Element {
    let result_form = Element::builder("x", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
        .append(form_type_field("result"))
        .append(
            Element::builder("field", NS_DATA_FORMS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "node")
                .append(
                    Element::builder("value", NS_DATA_FORMS)
                        .append(node_id)
                        .build(),
                )
                .build(),
        )
        .append(
            Element::builder("field", NS_DATA_FORMS)
                .attr(minidom::rxml::xml_ncname!("var").to_owned(), "device-id")
                .append(
                    Element::builder("value", NS_DATA_FORMS)
                        .append(device_id)
                        .build(),
                )
                .build(),
        )
        .build();
    let command = Element::builder("command", NS_COMMANDS)
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            REGISTER_DEVICE_NODE,
        )
        .attr(minidom::rxml::xml_ncname!("status").to_owned(), "completed")
        .append(result_form)
        .build();
    iq_with_command(command)
}

#[tokio::test]
async fn register_push_device_completes_multi_step_dance() {
    let driver = ScriptedDriver::new(vec![
        Ok(executing_response_with_form("session-1")),
        Ok(completed_response_with_outcome("node-xyz", "device-abc")),
    ]);
    let credentials = PushDeviceCredentials::WebPush {
        endpoint: "https://fcm.googleapis.com/wp/abc".to_string(),
        p256dh: "p256-key".to_string(),
        auth: "auth-secret".to_string(),
    };
    let outcome = register_push_device(
        &driver,
        "push.example.com",
        "app-web",
        PushEnvironment::Production,
        &credentials,
    )
    .await
    .expect("composer succeeds");
    assert_eq!(outcome.node.as_str(), "node-xyz");
    assert_eq!(outcome.device_id, "device-abc");

    let transcript = driver.transcript();
    assert_eq!(transcript.len(), 2, "two IQs on the wire");

    // Both IQs MUST be addressed to the configured Push Service JID.
    // A bug that swapped `to`/`from`, dropped `to`, or routed through
    // a transformed JID would leave the registration targeting the
    // wrong entity.
    assert_eq!(transcript[0].attr("to"), Some("push.example.com"));
    assert_eq!(transcript[0].attr("type"), Some("set"));
    assert_eq!(transcript[1].attr("to"), Some("push.example.com"));
    assert_eq!(transcript[1].attr("type"), Some("set"));

    // Stage 1: execute, no session id, no submitted form.
    let cmd1 = transcript[0]
        .get_child("command", NS_COMMANDS)
        .expect("stage 1 command");
    assert_eq!(cmd1.attr("node"), Some(REGISTER_DEVICE_NODE));
    assert_eq!(cmd1.attr("action"), Some("execute"));
    assert_eq!(cmd1.attr("sessionid"), None);
    assert!(cmd1.get_child("x", NS_DATA_FORMS).is_none());

    // Stage 3: complete, carries the propagated session id and the
    // platform-discriminated submit form.
    let cmd2 = transcript[1]
        .get_child("command", NS_COMMANDS)
        .expect("stage 3 command");
    assert_eq!(cmd2.attr("action"), Some("complete"));
    assert_eq!(cmd2.attr("sessionid"), Some("session-1"));
    let submitted = cmd2.get_child("x", NS_DATA_FORMS).expect("submitted form");
    assert_eq!(submitted.attr("type"), Some("submit"));

    // The submitted form carries platform-discriminated fields.
    let fields: std::collections::HashMap<String, String> = submitted
        .children()
        .filter(|c| c.name() == "field" && c.ns() == NS_DATA_FORMS)
        .filter_map(|f| {
            Some((
                f.attr("var")?.to_string(),
                f.get_child("value", NS_DATA_FORMS)?.text(),
            ))
        })
        .collect();
    assert_eq!(fields.get("platform").map(String::as_str), Some("web"));
    assert_eq!(fields.get("environment").map(String::as_str), Some("prod"));
    assert_eq!(fields.get("app-id").map(String::as_str), Some("app-web"));
    assert_eq!(
        fields.get("web-push-endpoint").map(String::as_str),
        Some("https://fcm.googleapis.com/wp/abc")
    );
    assert_eq!(
        fields.get("web-push-p256dh").map(String::as_str),
        Some("p256-key")
    );
    assert_eq!(
        fields.get("web-push-auth").map(String::as_str),
        Some("auth-secret")
    );
    assert!(!fields.contains_key("apns-token"));
    assert!(!fields.contains_key("fcm-token"));
}

#[tokio::test]
async fn register_push_device_propagates_transport_error_from_stage_1() {
    let driver = ScriptedDriver::new(vec![Err(ClientError::Disconnected)]);
    let credentials = PushDeviceCredentials::Apns {
        device_token: "apns-token".to_string(),
    };
    let err = register_push_device(
        &driver,
        "push.example.com",
        "app-ios",
        PushEnvironment::Sandbox,
        &credentials,
    )
    .await
    .expect_err("transport error propagates");
    assert!(matches!(err, ClientError::Disconnected));
    // We MUST NOT have sent stage 3 after stage 1 failed.
    assert_eq!(driver.transcript().len(), 1);
}

#[tokio::test]
async fn register_push_device_rejects_stage_2_without_session_id() {
    // XEP-0050 §3: an executing response without a sessionid violates
    // the spec — the composer must reject rather than make up an id.
    let bogus = {
        let command = Element::builder("command", NS_COMMANDS)
            .attr(
                minidom::rxml::xml_ncname!("node").to_owned(),
                REGISTER_DEVICE_NODE,
            )
            .attr(minidom::rxml::xml_ncname!("status").to_owned(), "executing")
            .build();
        iq_with_command(command)
    };
    let driver = ScriptedDriver::new(vec![Ok(bogus)]);
    let credentials = PushDeviceCredentials::Fcm {
        registration_token: "t".to_string(),
    };
    let err = register_push_device(
        &driver,
        "push.example.com",
        "app-android",
        PushEnvironment::Production,
        &credentials,
    )
    .await
    .expect_err("missing sessionid rejected");
    match err {
        ClientError::StanzaError(stanza_err) => {
            assert!(
                stanza_err
                    .text
                    .as_deref()
                    .unwrap_or("")
                    .contains("sessionid"),
                "diagnostic text must name the missing sessionid; got: {stanza_err:?}"
            );
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn register_push_device_rejects_completed_without_device_id_field() {
    // A result form that carries `node` but no `device-id` is a
    // server-side bug — the chat would not be able to scope a
    // future `disable-device` opt-out. The composer rejects rather
    // than persisting a half-populated record.
    let completed_without_device_id = {
        let result_form = Element::builder("x", NS_DATA_FORMS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .append(form_type_field("result"))
            .append(
                Element::builder("field", NS_DATA_FORMS)
                    .attr(minidom::rxml::xml_ncname!("var").to_owned(), "node")
                    .append(
                        Element::builder("value", NS_DATA_FORMS)
                            .append("node-xyz")
                            .build(),
                    )
                    .build(),
            )
            .build();
        let command = Element::builder("command", NS_COMMANDS)
            .attr(
                minidom::rxml::xml_ncname!("node").to_owned(),
                REGISTER_DEVICE_NODE,
            )
            .attr(minidom::rxml::xml_ncname!("status").to_owned(), "completed")
            .append(result_form)
            .build();
        iq_with_command(command)
    };
    let driver = ScriptedDriver::new(vec![
        Ok(executing_response_with_form("session-1")),
        Ok(completed_without_device_id),
    ]);
    let credentials = PushDeviceCredentials::Apns {
        device_token: "t".to_string(),
    };
    let err = register_push_device(
        &driver,
        "push.example.com",
        "app-ios",
        PushEnvironment::Sandbox,
        &credentials,
    )
    .await
    .expect_err("missing device-id field rejected");
    assert!(matches!(err, ClientError::StanzaError(_)));
}

#[tokio::test]
async fn register_push_device_rejects_completed_without_node_field() {
    let completed_without_node = {
        let result_form = Element::builder("x", NS_DATA_FORMS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "result")
            .append(form_type_field("result"))
            .build();
        let command = Element::builder("command", NS_COMMANDS)
            .attr(
                minidom::rxml::xml_ncname!("node").to_owned(),
                REGISTER_DEVICE_NODE,
            )
            .attr(minidom::rxml::xml_ncname!("status").to_owned(), "completed")
            .append(result_form)
            .build();
        iq_with_command(command)
    };
    let driver = ScriptedDriver::new(vec![
        Ok(executing_response_with_form("session-1")),
        Ok(completed_without_node),
    ]);
    let credentials = PushDeviceCredentials::WebPush {
        endpoint: "e".to_string(),
        p256dh: "p".to_string(),
        auth: "a".to_string(),
    };
    let err = register_push_device(
        &driver,
        "push.example.com",
        "app-web",
        PushEnvironment::Production,
        &credentials,
    )
    .await
    .expect_err("missing node field rejected");
    match err {
        ClientError::StanzaError(stanza_err) => {
            assert!(stanza_err.text.as_deref().unwrap_or("").contains("'node'"));
        }
        other => panic!("unexpected error: {other:?}"),
    }
}

#[tokio::test]
async fn register_push_device_rejects_stage_4_canceled_status() {
    // XEP-0050 §3 allows the responder to return `status='canceled'`
    // at any stage — the composer MUST treat that as a failed
    // registration rather than mistaking it for `completed`.
    let canceled = {
        let command = Element::builder("command", NS_COMMANDS)
            .attr(
                minidom::rxml::xml_ncname!("node").to_owned(),
                REGISTER_DEVICE_NODE,
            )
            .attr(minidom::rxml::xml_ncname!("status").to_owned(), "canceled")
            .build();
        iq_with_command(command)
    };
    let driver = ScriptedDriver::new(vec![
        Ok(executing_response_with_form("session-1")),
        Ok(canceled),
    ]);
    let credentials = PushDeviceCredentials::WebPush {
        endpoint: "e".to_string(),
        p256dh: "p".to_string(),
        auth: "a".to_string(),
    };
    let err = register_push_device(
        &driver,
        "push.example.com",
        "app-web",
        PushEnvironment::Production,
        &credentials,
    )
    .await
    .expect_err("stage 4 canceled rejected");
    assert!(matches!(err, ClientError::StanzaError(_)));
}

#[tokio::test]
async fn register_push_device_rejects_stage_4_still_executing() {
    // A responder that mistakenly returned `status='executing'` again
    // at stage 4 (instead of `completed`) — the composer must reject
    // rather than loop or treat as success.
    let still_executing = {
        let command = Element::builder("command", NS_COMMANDS)
            .attr(
                minidom::rxml::xml_ncname!("node").to_owned(),
                REGISTER_DEVICE_NODE,
            )
            .attr(
                minidom::rxml::xml_ncname!("sessionid").to_owned(),
                "session-1",
            )
            .attr(minidom::rxml::xml_ncname!("status").to_owned(), "executing")
            .build();
        iq_with_command(command)
    };
    let driver = ScriptedDriver::new(vec![
        Ok(executing_response_with_form("session-1")),
        Ok(still_executing),
    ]);
    let credentials = PushDeviceCredentials::Apns {
        device_token: "t".to_string(),
    };
    let err = register_push_device(
        &driver,
        "push.example.com",
        "app-ios",
        PushEnvironment::Sandbox,
        &credentials,
    )
    .await
    .expect_err("stage 4 still-executing rejected");
    assert!(matches!(err, ClientError::StanzaError(_)));
}
