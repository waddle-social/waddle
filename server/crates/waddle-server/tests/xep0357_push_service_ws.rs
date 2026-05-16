//! XEP-0357 Push Service wire-conformance tests over WebSocket.

mod ws_common;

use std::str::FromStr;

use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const CLIENT_NS: &str = "jabber:client";
const DISCO_INFO_NS: &str = "http://jabber.org/protocol/disco#info";
const DISCO_ITEMS_NS: &str = "http://jabber.org/protocol/disco#items";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUSH: &str = "urn:xmpp:push:0";
const NS_WADDLE_PUSH_SERVICE: &str = "urn:waddle:push-service:0";
const STANZA_ERROR_NS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const PUSH_SERVICE_JID: &str = "push.localhost";

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();
    let client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("xep0357-push-service-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("websocket connection");
    (server, client)
}

fn element_to_xml(element: Element) -> String {
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize XML element");
    String::from_utf8(buf).expect("minidom serializes UTF-8")
}

fn iq_frame(iq_type: &str, id: &str, to: &str, payload: Element) -> String {
    element_to_xml(
        Element::builder("iq", CLIENT_NS)
            .attr("type", iq_type)
            .attr("id", id)
            .attr("to", to)
            .append(payload)
            .build(),
    )
}

async fn send_iq(client: &mut WsXmppClient, frame: String, id: &str) -> String {
    client.send(&frame).await.expect("send iq");
    client
        .recv_matching(|candidate| candidate.contains(id) && candidate.contains("<iq"))
        .await
        .expect("iq response")
}

fn child_attr(xml: &str, child_name: &str, attr: &str) -> Option<String> {
    let element = Element::from_str(xml).ok()?;
    element
        .children()
        .find(|child| child.name() == child_name)
        .and_then(|child| child.attr(attr))
        .map(str::to_string)
}

fn parse_iq_element(xml: &str, id: &str, iq_type: &str) -> Element {
    let element = Element::from_str(xml).expect("valid XML response");
    assert_eq!(element.name(), "iq");
    assert_eq!(element.ns(), CLIENT_NS);
    assert_eq!(element.attr("id"), Some(id));
    assert_eq!(element.attr("type"), Some(iq_type));
    element
}

fn single_child<'a>(parent: &'a Element, name: &str, ns: &str) -> &'a Element {
    let mut children = parent.children();
    let child = children.next().expect("response child");
    assert!(
        children.next().is_none(),
        "expected exactly one child in {parent:?}"
    );
    assert_eq!(child.name(), name);
    assert_eq!(child.ns(), ns);
    child
}

fn disco_feature_vars(query: &Element) -> std::collections::BTreeSet<String> {
    query
        .children()
        .filter(|child| child.name() == "feature" && child.ns() == DISCO_INFO_NS)
        .filter_map(|child| child.attr("var").map(str::to_string))
        .collect()
}

fn assert_iq_error_condition(xml: &str, id: &str, condition: &str) {
    let iq = parse_iq_element(xml, id, "error");
    let error = single_child(&iq, "error", CLIENT_NS);
    assert!(
        error
            .children()
            .any(|child| child.name() == condition && child.ns() == STANZA_ERROR_NS),
        "expected {condition} stanza error: {xml}"
    );
}

#[tokio::test]
async fn xep0357_push_service_disco_identifies_as_pubsub_push() {
    let (_server, mut client) = setup().await;
    let query = Element::builder("query", DISCO_INFO_NS).build();

    let response = send_iq(
        &mut client,
        iq_frame("get", "push-disco-info", PUSH_SERVICE_JID, query),
        "push-disco-info",
    )
    .await;

    let iq = parse_iq_element(&response, "push-disco-info", "result");
    let query = single_child(&iq, "query", DISCO_INFO_NS);
    assert!(
        query.children().any(|child| {
            child.name() == "identity"
                && child.ns() == DISCO_INFO_NS
                && child.attr("category") == Some("pubsub")
                && child.attr("type") == Some("push")
        }),
        "XEP-0357 Push Service MUST identify as pubsub/push: {response}"
    );
    let features = disco_feature_vars(query);
    assert!(
        features.contains(NS_PUSH),
        "XEP-0357 Push Service MUST advertise urn:xmpp:push:0: {response}"
    );
    assert!(
        features.contains("http://jabber.org/protocol/pubsub#publish"),
        "XEP-0357 Push Service MUST support PubSub publish: {response}"
    );
    assert!(
        features.contains("http://jabber.org/protocol/pubsub#access-whitelist"),
        "XEP-0357 Push Service MUST default to whitelist access: {response}"
    );
    assert!(
        features.contains("http://jabber.org/protocol/pubsub#publish-only-affiliation"),
        "XEP-0357 Push Service MUST support publish-only affiliation: {response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn xep0357_push_service_rejects_client_origin_pubsub_notification_publish() {
    let (_server, mut client) = setup().await;

    let ensure_node = Element::builder("ensure-node", NS_WADDLE_PUSH_SERVICE)
        .attr("app-id", "web")
        .build();
    let node_response = send_iq(
        &mut client,
        iq_frame("set", "push-ensure-node", PUSH_SERVICE_JID, ensure_node),
        "push-ensure-node",
    )
    .await;
    let node = child_attr(&node_response, "node", "id").expect("node id");

    let items_query = Element::builder("query", DISCO_ITEMS_NS).build();
    let items_response = send_iq(
        &mut client,
        iq_frame("get", "push-disco-items", PUSH_SERVICE_JID, items_query),
        "push-disco-items",
    )
    .await;
    let items_iq = parse_iq_element(&items_response, "push-disco-items", "result");
    let items_query = single_child(&items_iq, "query", DISCO_ITEMS_NS);
    let items: Vec<&Element> = items_query
        .children()
        .filter(|child| child.name() == "item" && child.ns() == DISCO_ITEMS_NS)
        .collect();
    assert_eq!(
        items.len(),
        1,
        "Push Service disco#items should expose exactly the owner's durable node: {items_response}"
    );
    assert_eq!(items[0].attr("jid"), Some(PUSH_SERVICE_JID));
    assert_eq!(items[0].attr("node"), Some(node.as_str()));

    let register_device = Element::builder("register-device", NS_WADDLE_PUSH_SERVICE)
        .attr("node", node.as_str())
        .attr("device-id", "web-1")
        .attr("platform", "web")
        .attr("environment", "test")
        .append(
            Element::builder("provider-endpoint", NS_WADDLE_PUSH_SERVICE)
                .append("https://push.example.com/endpoint")
                .build(),
        )
        .append(
            Element::builder("provider-token", NS_WADDLE_PUSH_SERVICE)
                .append("provider-secret")
                .build(),
        )
        .build();
    let register_response = send_iq(
        &mut client,
        iq_frame(
            "set",
            "push-register-device",
            PUSH_SERVICE_JID,
            register_device,
        ),
        "push-register-device",
    )
    .await;
    let register_iq = parse_iq_element(&register_response, "push-register-device", "result");
    let device = single_child(&register_iq, "device", NS_WADDLE_PUSH_SERVICE);
    assert_eq!(device.attr("status"), Some("active"));

    let notification = Element::builder("notification", NS_PUSH).build();
    let item = Element::builder("item", NS_PUBSUB)
        .attr("id", "push-1")
        .append(notification)
        .build();
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr("node", node.as_str())
        .append(item)
        .build();
    let pubsub = Element::builder("pubsub", NS_PUBSUB)
        .append(publish)
        .build();
    let publish_response = send_iq(
        &mut client,
        iq_frame("set", "push-publish", PUSH_SERVICE_JID, pubsub),
        "push-publish",
    )
    .await;

    assert_iq_error_condition(&publish_response, "push-publish", "forbidden");

    let _ = client.close().await;
}

#[tokio::test]
async fn xep0357_first_party_enable_requires_owned_active_node_with_device() {
    let (_server, mut client) = setup().await;

    let enable_unknown_node = Element::builder("enable", NS_PUSH)
        .attr("jid", PUSH_SERVICE_JID)
        .attr("node", "missing-web-node")
        .build();
    let unknown_node_response = send_iq(
        &mut client,
        iq_frame(
            "set",
            "push-enable-unknown-node",
            DOMAIN,
            enable_unknown_node,
        ),
        "push-enable-unknown-node",
    )
    .await;
    assert_iq_error_condition(
        &unknown_node_response,
        "push-enable-unknown-node",
        "item-not-found",
    );

    let ensure_node = Element::builder("ensure-node", NS_WADDLE_PUSH_SERVICE)
        .attr("app-id", "web")
        .build();
    let node_response = send_iq(
        &mut client,
        iq_frame(
            "set",
            "push-enable-ensure-node",
            PUSH_SERVICE_JID,
            ensure_node,
        ),
        "push-enable-ensure-node",
    )
    .await;
    let node = child_attr(&node_response, "node", "id").expect("node id");

    let enable_without_device = Element::builder("enable", NS_PUSH)
        .attr("jid", PUSH_SERVICE_JID)
        .attr("node", node.as_str())
        .build();
    let missing_device_response = send_iq(
        &mut client,
        iq_frame(
            "set",
            "push-enable-without-device",
            DOMAIN,
            enable_without_device,
        ),
        "push-enable-without-device",
    )
    .await;
    assert_iq_error_condition(
        &missing_device_response,
        "push-enable-without-device",
        "bad-request",
    );

    let register_device = Element::builder("register-device", NS_WADDLE_PUSH_SERVICE)
        .attr("node", node.as_str())
        .attr("device-id", "web-1")
        .attr("platform", "web")
        .attr("environment", "test")
        .append(
            Element::builder("provider-token", NS_WADDLE_PUSH_SERVICE)
                .append("provider-secret")
                .build(),
        )
        .build();
    let register_response = send_iq(
        &mut client,
        iq_frame(
            "set",
            "push-enable-register-device",
            PUSH_SERVICE_JID,
            register_device,
        ),
        "push-enable-register-device",
    )
    .await;
    let _ = parse_iq_element(&register_response, "push-enable-register-device", "result");

    let enable = Element::builder("enable", NS_PUSH)
        .attr("jid", PUSH_SERVICE_JID)
        .attr("node", node.as_str())
        .build();
    let enable_response = send_iq(
        &mut client,
        iq_frame("set", "push-enable-first-party", DOMAIN, enable),
        "push-enable-first-party",
    )
    .await;
    let enable_iq = parse_iq_element(&enable_response, "push-enable-first-party", "result");
    assert!(enable_iq.children().next().is_none());

    let disable = Element::builder("disable", NS_PUSH)
        .attr("jid", PUSH_SERVICE_JID)
        .attr("node", node.as_str())
        .build();
    let disable_response = send_iq(
        &mut client,
        iq_frame("set", "push-disable-first-party", DOMAIN, disable),
        "push-disable-first-party",
    )
    .await;
    let disable_iq = parse_iq_element(&disable_response, "push-disable-first-party", "result");
    assert!(disable_iq.children().next().is_none());

    let enable_with_publish_options = Element::builder("enable", NS_PUSH)
        .attr("jid", PUSH_SERVICE_JID)
        .attr("node", node.as_str())
        .append(
            Element::builder("x", waddle_xmpp::xep::NS_DATA_FORMS)
                .attr("type", "submit")
                .append(
                    Element::builder("field", waddle_xmpp::xep::NS_DATA_FORMS)
                        .attr("var", "FORM_TYPE")
                        .append(
                            Element::builder("value", waddle_xmpp::xep::NS_DATA_FORMS)
                                .append(waddle_xmpp::xep::NS_PUBSUB_PUBLISH_OPTIONS)
                                .build(),
                        )
                        .build(),
                )
                .append(
                    Element::builder("field", waddle_xmpp::xep::NS_DATA_FORMS)
                        .attr("var", "unlisted-secret-like-field")
                        .append(
                            Element::builder("value", waddle_xmpp::xep::NS_DATA_FORMS)
                                .append("opaque-provider-secret")
                                .build(),
                        )
                        .build(),
                )
                .build(),
        )
        .build();
    let publish_options_response = send_iq(
        &mut client,
        iq_frame(
            "set",
            "push-enable-first-party-publish-options",
            DOMAIN,
            enable_with_publish_options,
        ),
        "push-enable-first-party-publish-options",
    )
    .await;
    let publish_options_iq = parse_iq_element(
        &publish_options_response,
        "push-enable-first-party-publish-options",
        "result",
    );
    assert!(publish_options_iq.children().next().is_none());

    let _ = client.close().await;
}

#[tokio::test]
async fn xep0357_first_party_enable_rejects_foreign_push_service_node() {
    let server = TestServer::start_with_extra_accounts(&[("bob", "bob-password")]);
    let ws_url = server.ws_url();
    let admin_password = server.fixed_account_password().to_string();
    let mut admin = WsXmppClient::connect_and_auth(
        &ws_url,
        DOMAIN,
        USERNAME,
        &admin_password,
        &format!("xep0357-admin-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("admin connection");
    let mut bob = WsXmppClient::connect_and_auth(
        &ws_url,
        DOMAIN,
        "bob",
        "bob-password",
        &format!("xep0357-bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");

    let bob_node_response = send_iq(
        &mut bob,
        iq_frame(
            "set",
            "bob-push-ensure-node",
            PUSH_SERVICE_JID,
            Element::builder("ensure-node", NS_WADDLE_PUSH_SERVICE)
                .attr("app-id", "web")
                .build(),
        ),
        "bob-push-ensure-node",
    )
    .await;
    let bob_node = child_attr(&bob_node_response, "node", "id").expect("bob node id");
    let bob_device = Element::builder("register-device", NS_WADDLE_PUSH_SERVICE)
        .attr("node", bob_node.as_str())
        .attr("device-id", "bob-web-1")
        .attr("platform", "web")
        .attr("environment", "test")
        .append(
            Element::builder("provider-token", NS_WADDLE_PUSH_SERVICE)
                .append("bob-provider-secret")
                .build(),
        )
        .build();
    let _ = send_iq(
        &mut bob,
        iq_frame(
            "set",
            "bob-push-register-device",
            PUSH_SERVICE_JID,
            bob_device,
        ),
        "bob-push-register-device",
    )
    .await;

    let admin_enable = Element::builder("enable", NS_PUSH)
        .attr("jid", PUSH_SERVICE_JID)
        .attr("node", bob_node.as_str())
        .build();
    let admin_response = send_iq(
        &mut admin,
        iq_frame("set", "admin-enable-bob-node", DOMAIN, admin_enable),
        "admin-enable-bob-node",
    )
    .await;
    assert_iq_error_condition(&admin_response, "admin-enable-bob-node", "forbidden");

    let _ = bob.close().await;
    let _ = admin.close().await;
}

#[tokio::test]
async fn xep0357_push_service_rejects_oversized_custom_node_request() {
    let (_server, mut client) = setup().await;
    let ensure_node = Element::builder("ensure-node", NS_WADDLE_PUSH_SERVICE)
        .attr("app-id", "x".repeat(129))
        .build();

    let response = send_iq(
        &mut client,
        iq_frame(
            "set",
            "push-ensure-node-too-large",
            PUSH_SERVICE_JID,
            ensure_node,
        ),
        "push-ensure-node-too-large",
    )
    .await;

    assert_iq_error_condition(&response, "push-ensure-node-too-large", "bad-request");

    let _ = client.close().await;
}
