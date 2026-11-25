//! XEP-0503 §"Node configuration": a Space node SHOULD have
//! `pubsub#notify_sub` and `pubsub#notify_config` set to true. These tests
//! hold the service to both: the owner is told of subscription changes
//! (XEP-0060 §8.9.2, subscription event shape of §12.20), and subscribers are
//! told of configuration changes (XEP-0060 §8.2.4 "Success With
//! Notifications").

use waddle_ws_test_support as ws_common;

use std::time::Duration;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const SPACES_JID: &str = "spaces.localhost";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUBSUB_OWNER: &str = "http://jabber.org/protocol/pubsub#owner";
const NS_PUBSUB_EVENT: &str = "http://jabber.org/protocol/pubsub#event";
const NS_DATA: &str = "jabber:x:data";
const NODE_CONFIG_FORM: &str = "http://jabber.org/protocol/pubsub#node_config";
const WAIT: Duration = Duration::from_secs(3);

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn connect(server: &TestServer, user: &str, password: &str, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, user, password, resource)
        .await
        .expect("connect")
}

async fn iq(client: &mut WsXmppClient, kind: &str, id: &str, body: &str) -> String {
    client
        .send(&format!(
            r#"<iq type="{kind}" id="{id}" to="{SPACES_JID}">{body}</iq>"#
        ))
        .await
        .expect("send iq");
    client
        .recv_matching(|frame| frame.contains(&format!("id='{id}'")) && frame.contains("<iq"))
        .await
        .expect("iq response")
}

fn is_result(frame: &str) -> bool {
    frame.contains("type='result'")
}

/// The next pubsub event message whose first child is `child` for `node`.
async fn event(client: &mut WsXmppClient, child: &str) -> Option<Element> {
    let deadline = std::time::Instant::now() + WAIT;
    loop {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        if remaining.is_zero() {
            return None;
        }
        let frame = client.recv_timeout(remaining).await.ok()?;
        if !frame.contains("<message") || !frame.contains(NS_PUBSUB_EVENT) {
            continue;
        }
        let message: Element = frame.parse().ok()?;
        let found = message
            .get_child("event", NS_PUBSUB_EVENT)
            .is_some_and(|event| event.get_child(child, NS_PUBSUB_EVENT).is_some());
        if found {
            return Some(message);
        }
    }
}

fn payload<'a>(message: &'a Element, child: &str) -> &'a Element {
    message
        .get_child("event", NS_PUBSUB_EVENT)
        .and_then(|event| event.get_child(child, NS_PUBSUB_EVENT))
        .expect("event child")
}

fn field_value(form: &Element, var: &str) -> Option<String> {
    form.children()
        .find(|field| field.name() == "field" && field.attr("var") == Some(var))
        .and_then(|field| field.get_child("value", NS_DATA))
        .map(Element::text)
}

fn configure_form(node: &str, fields: &str) -> String {
    format!(
        r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="{node}"><x xmlns="{NS_DATA}" type="submit"><field var="FORM_TYPE" type="hidden"><value>{NODE_CONFIG_FORM}</value></field>{fields}</x></configure></pubsub>"#
    )
}

#[tokio::test]
async fn space_configuration_form_advertises_both_notification_options() {
    let _serial = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();
    let mut admin = connect(&server, "admin", &password, "form").await;

    let form = iq(
        &mut admin,
        "get",
        "cfg-get",
        &format!(r#"<pubsub xmlns="{NS_PUBSUB_OWNER}"><configure node="general"/></pubsub>"#),
    )
    .await;
    let form: Element = form.parse().expect("iq parses");
    let x = form
        .get_child("pubsub", NS_PUBSUB_OWNER)
        .and_then(|pubsub| pubsub.get_child("configure", NS_PUBSUB_OWNER))
        .and_then(|configure| configure.get_child("x", NS_DATA))
        .expect("configuration form");
    assert_eq!(
        field_value(x, "pubsub#notify_sub").as_deref(),
        Some("1"),
        "XEP-0503: notify_sub SHOULD be true"
    );
    assert_eq!(
        field_value(x, "pubsub#notify_config").as_deref(),
        Some("1"),
        "XEP-0503: notify_config SHOULD be true"
    );
    let _ = admin.close().await;
}

#[tokio::test]
async fn space_owner_is_told_when_an_entity_subscribes_and_unsubscribes() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);
    let password = server.fixed_account_password().to_string();
    let mut admin = connect(&server, "admin", &password, "owner").await;
    let mut alice = connect(&server, "alice", &alice_password, "phone").await;

    let subscribed = iq(
        &mut alice,
        "set",
        "alice-sub",
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="general" jid="alice@{DOMAIN}"/></pubsub>"#
        ),
    )
    .await;
    assert!(is_result(&subscribed), "alice subscribes: {subscribed}");
    let subid = subscribed
        .parse::<Element>()
        .ok()
        .and_then(|iq| {
            iq.get_child("pubsub", NS_PUBSUB)
                .and_then(|pubsub| pubsub.get_child("subscription", NS_PUBSUB))
                .and_then(|subscription| subscription.attr("subid").map(str::to_owned))
        })
        .expect("subscribe result carries a subid");

    let notice = event(&mut admin, "subscription")
        .await
        .expect("the owner is notified of the new subscription (pubsub#notify_sub)");
    assert_eq!(notice.attr("from"), Some(SPACES_JID));
    assert_eq!(notice.attr("type"), Some("headline"));
    let subscription = payload(&notice, "subscription");
    assert_eq!(subscription.attr("node"), Some("general"));
    assert_eq!(
        subscription.attr("jid"),
        Some(format!("alice@{DOMAIN}").as_str())
    );
    assert_eq!(subscription.attr("subscription"), Some("subscribed"));
    assert_eq!(subscription.attr("subid"), Some(subid.as_str()));

    let unsubscribed = iq(
        &mut alice,
        "set",
        "alice-unsub",
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><unsubscribe node="general" jid="alice@{DOMAIN}" subid="{subid}"/></pubsub>"#
        ),
    )
    .await;
    assert!(
        is_result(&unsubscribed),
        "alice unsubscribes: {unsubscribed}"
    );
    let notice = event(&mut admin, "subscription")
        .await
        .expect("the owner is notified of the unsubscribe");
    let subscription = payload(&notice, "subscription");
    assert_eq!(subscription.attr("node"), Some("general"));
    assert_eq!(subscription.attr("subscription"), Some("none"));
    assert_eq!(
        subscription.attr("jid"),
        Some(format!("alice@{DOMAIN}").as_str())
    );

    let _ = alice.close().await;
    let _ = admin.close().await;
}

#[tokio::test]
async fn space_subscribers_are_told_of_a_configuration_change() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);
    let password = server.fixed_account_password().to_string();
    let mut admin = connect(&server, "admin", &password, "owner").await;
    let mut alice = connect(&server, "alice", &alice_password, "phone").await;

    let subscribed = iq(
        &mut alice,
        "set",
        "alice-sub",
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="general" jid="alice@{DOMAIN}"/></pubsub>"#
        ),
    )
    .await;
    assert!(is_result(&subscribed), "alice subscribes: {subscribed}");

    // A Space delivers payloads, so the configuration as a `result` data form.
    let changed = iq(
        &mut admin,
        "set",
        "cfg-on",
        &configure_form(
            "general",
            r#"<field var="pubsub#max_items"><value>200</value></field>"#,
        ),
    )
    .await;
    assert!(is_result(&changed), "owner configures: {changed}");
    let notice = event(&mut alice, "configuration")
        .await
        .expect("subscriber is notified of the second change");
    let configuration = payload(&notice, "configuration");
    assert_eq!(configuration.attr("node"), Some("general"));
    let form = configuration
        .get_child("x", NS_DATA)
        .expect("full-payload configuration event carries the form");
    assert_eq!(form.attr("type"), Some("result"));
    assert_eq!(
        field_value(form, "FORM_TYPE").as_deref(),
        Some(NODE_CONFIG_FORM)
    );
    assert_eq!(
        field_value(form, "pubsub#deliver_payloads").as_deref(),
        Some("1")
    );
    assert_eq!(field_value(form, "pubsub#notify_sub").as_deref(), Some("1"));
    assert_eq!(
        field_value(form, "pubsub#notify_config").as_deref(),
        Some("1")
    );

    let _ = alice.close().await;
    let _ = admin.close().await;
}

#[tokio::test]
async fn space_subscribers_hear_nothing_when_a_submission_changes_nothing() {
    let _serial = TEST_SERIAL.lock().await;
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("alice", &alice_password)]);
    let password = server.fixed_account_password().to_string();
    let mut admin = connect(&server, "admin", &password, "owner").await;
    let mut alice = connect(&server, "alice", &alice_password, "phone").await;
    let subscribed = iq(
        &mut alice,
        "set",
        "alice-sub",
        &format!(
            r#"<pubsub xmlns="{NS_PUBSUB}"><subscribe node="general" jid="alice@{DOMAIN}"/></pubsub>"#
        ),
    )
    .await;
    assert!(is_result(&subscribed), "alice subscribes: {subscribed}");

    let same = iq(
        &mut admin,
        "set",
        "cfg-same",
        &configure_form(
            "general",
            r#"<field var="pubsub#deliver_payloads"><value>1</value></field>"#,
        ),
    )
    .await;
    assert!(is_result(&same), "owner configures: {same}");
    assert!(
        event(&mut alice, "configuration").await.is_none(),
        "an unchanged configuration is not a configuration change"
    );
    let _ = alice.close().await;
    let _ = admin.close().await;
}
