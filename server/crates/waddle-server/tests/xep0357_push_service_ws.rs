//! XEP-0357 Push Service wire-conformance tests over WebSocket.

mod ws_common;

use std::{
    str::FromStr,
    time::{Duration, Instant},
};

use sqlx::Row;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const CLIENT_NS: &str = "jabber:client";
const DISCO_INFO_NS: &str = "http://jabber.org/protocol/disco#info";
const DISCO_ITEMS_NS: &str = "http://jabber.org/protocol/disco#items";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_DATA_FORMS: &str = "jabber:x:data";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUSH: &str = "urn:xmpp:push:0";
const NS_WADDLE_PUSH_CONTEXT: &str = "urn:waddle:push:context:0";
const STANZA_ERROR_NS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";

/// XEP-0050 node identifier for push device registration. Mirrors
/// `crate::push_service::commands::REGISTER_DEVICE_NODE`.
const REGISTER_DEVICE_NODE: &str = "register-device";
/// XEP-0050 node identifier for push device deregistration.
const DISABLE_DEVICE_NODE: &str = "disable-device";
/// FORM_TYPE the server expects on the `register-device` submit form.
const REGISTER_DEVICE_FORM_TYPE: &str = "urn:xmpp:push-service:commands:register-device:0";
/// FORM_TYPE the server expects on the `disable-device` submit form.
const DISABLE_DEVICE_FORM_TYPE: &str = "urn:xmpp:push-service:commands:disable-device:0";

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
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), iq_type)
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
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

fn parse_iq_element(xml: &str, id: &str, iq_type: &str) -> Element {
    let element = Element::from_str(xml).unwrap_or_else(|err| {
        panic!("invalid XML response (err={err}): {xml}");
    });
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

fn xdata_field_value(form: &Element, var: &str) -> Option<String> {
    form.children()
        .find(|child| {
            child.is("field", waddle_xmpp::xep::NS_DATA_FORMS) && child.attr("var") == Some(var)
        })
        .and_then(|field| {
            field
                .children()
                .find(|child| child.is("value", waddle_xmpp::xep::NS_DATA_FORMS))
        })
        .map(Element::text)
}

fn assert_iq_error_condition(xml: &str, id: &str, condition: &str) {
    // Two error envelopes flow through the server: typed
    // `build_iq_error_xml_typed` and stringly `generate_iq_error`. The
    // typed one stamps `xmlns='jabber:client'` on the `<iq/>`, the
    // stringly one omits it. Parsing the latter with `Element::from_str`
    // fails on `MissingNamespace`, so fall back to a substring match
    // when minidom rejects the frame outright. We still want to assert
    // `type='error'` + the right condition element + the request id.
    if let Ok(element) = Element::from_str(xml) {
        assert_eq!(element.name(), "iq");
        assert_eq!(element.attr("id"), Some(id));
        assert_eq!(element.attr("type"), Some("error"));
        let error = element
            .children()
            .find(|child| child.name() == "error")
            .unwrap_or_else(|| panic!("error envelope missing in: {xml}"));
        assert!(
            error
                .children()
                .any(|child| child.name() == condition && child.ns() == STANZA_ERROR_NS),
            "expected {condition} stanza error: {xml}"
        );
        return;
    }
    assert!(
        xml.contains(&format!("id='{id}'")) || xml.contains(&format!("id=\"{id}\"")),
        "error response must echo the request id ({id}): {xml}"
    );
    assert!(
        xml.contains("type='error'") || xml.contains("type=\"error\""),
        "expected type='error': {xml}"
    );
    let condition_tag_open = format!("<{condition} ");
    let condition_tag_self_closing = format!("<{condition}/>");
    assert!(
        xml.contains(&condition_tag_open) || xml.contains(&condition_tag_self_closing),
        "expected <{condition}/> in error envelope: {xml}"
    );
}

async fn wait_for_push_publish_payload(database_url: &str, node: &str) -> String {
    let pool = sqlx::SqlitePool::connect(database_url)
        .await
        .expect("open sqlite db");
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Some(row) = sqlx::query(
            "SELECT payload_xml \
             FROM push_publish_jobs \
             WHERE node = ? \
             ORDER BY created_at_ms DESC \
             LIMIT 1",
        )
        .bind(node)
        .fetch_optional(&pool)
        .await
        .expect("query push publish job")
        {
            return row.get("payload_xml");
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for Push Service publish job for node {node}"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
}

async fn notification_candidate_count(database_url: &str, recipient: &str) -> i64 {
    let pool = sqlx::SqlitePool::connect(database_url)
        .await
        .expect("open sqlite db");
    let row = sqlx::query(
        "SELECT COUNT(*) AS count \
         FROM notification_candidates \
         WHERE recipient_bare_jid = ?",
    )
    .bind(recipient)
    .fetch_one(&pool)
    .await
    .expect("query notification candidates");
    row.get("count")
}

/// Build a XEP-0050 `<command/>` Element targeting the push service.
fn command_element(
    node: &str,
    action: &str,
    session_id: Option<&str>,
    submit_form: Option<Element>,
) -> Element {
    let mut command = Element::builder("command", NS_COMMANDS)
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
/// `FORM_TYPE` and a list of `(var, value)` text-single fields.
fn submit_form(form_type: &str, fields: &[(&str, &str)]) -> Element {
    let mut form = Element::builder("x", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit");
    form = form.append(form_field("FORM_TYPE", form_type, Some("hidden")));
    for (var, value) in fields {
        form = form.append(form_field(var, value, None));
    }
    form.build()
}

fn form_field(var: &str, value: &str, type_attr: Option<&str>) -> Element {
    let mut field = Element::builder("field", NS_DATA_FORMS)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var);
    if let Some(type_attr) = type_attr {
        field = field.attr(minidom::rxml::xml_ncname!("type").to_owned(), type_attr);
    }
    field
        .append(
            Element::builder("value", NS_DATA_FORMS)
                .append(value)
                .build(),
        )
        .build()
}

/// Outcome of a successful `register-device` XEP-0050 round. Both
/// fields are extracted from the stage-4 result form — the chat
/// client persists them so the matching `disable-device` round can
/// scope to the assigned device row. Most existing call sites only
/// need the node id and stay on
/// [`register_web_push_device_via_xep0050`]; the per-device disable
/// test uses [`register_web_push_device_via_xep0050_with_device_id`].
struct RegisterDeviceOutcome {
    node: String,
    device_id: String,
}

/// Drive the full 4-stage XEP-0050 `register-device` dance against
/// `push.<domain>` and return the assigned XEP-0357 node id. Used by
/// every WS test that needs a registered push device before it can
/// exercise XEP-0357 enable / publish flows.
async fn register_web_push_device_via_xep0050(
    client: &mut WsXmppClient,
    id_prefix: &str,
    app_id: &str,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
) -> String {
    register_web_push_device_via_xep0050_with_device_id(
        client, id_prefix, app_id, endpoint, p256dh, auth,
    )
    .await
    .node
}

async fn register_web_push_device_via_xep0050_with_device_id(
    client: &mut WsXmppClient,
    id_prefix: &str,
    app_id: &str,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
) -> RegisterDeviceOutcome {
    // Stage 1 → 2: execute, expect status='executing' + sessionid.
    let execute_id = format!("{id_prefix}-execute");
    let execute = command_element(REGISTER_DEVICE_NODE, "execute", None, None);
    let executing_response = send_iq(
        client,
        iq_frame("set", &execute_id, PUSH_SERVICE_JID, execute),
        &execute_id,
    )
    .await;
    let executing_iq = parse_iq_element(&executing_response, &execute_id, "result");
    let executing_command = single_child(&executing_iq, "command", NS_COMMANDS);
    assert_eq!(
        executing_command.attr("status"),
        Some("executing"),
        "stage 2 must carry status='executing': {executing_response}"
    );
    let session_id = executing_command
        .attr("sessionid")
        .expect("XEP-0050 §3 sessionid")
        .to_string();

    // Stage 3 → 4: complete with the platform-discriminated form,
    // expect status='completed' + result form carrying `node`.
    let complete_id = format!("{id_prefix}-complete");
    let form = submit_form(
        REGISTER_DEVICE_FORM_TYPE,
        &[
            ("platform", "web"),
            ("environment", "prod"),
            ("app_id", app_id),
            ("web-push-endpoint", endpoint),
            ("web-push-p256dh", p256dh),
            ("web-push-auth", auth),
        ],
    );
    let complete = command_element(
        REGISTER_DEVICE_NODE,
        "complete",
        Some(&session_id),
        Some(form),
    );
    let completed_response = send_iq(
        client,
        iq_frame("set", &complete_id, PUSH_SERVICE_JID, complete),
        &complete_id,
    )
    .await;
    let completed_iq = parse_iq_element(&completed_response, &complete_id, "result");
    let completed_command = single_child(&completed_iq, "command", NS_COMMANDS);
    assert_eq!(
        completed_command.attr("status"),
        Some("completed"),
        "stage 4 must carry status='completed': {completed_response}"
    );
    let result_form = completed_command
        .children()
        .find(|child| child.is("x", NS_DATA_FORMS))
        .expect("stage 4 result form");
    let node = xdata_field_value(result_form, "node")
        .expect("stage 4 result form must carry the `node` field");
    let device_id = xdata_field_value(result_form, "device-id")
        .expect("stage 4 result form must carry the `device-id` field");
    RegisterDeviceOutcome { node, device_id }
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
async fn push_service_disco_advertises_xep0128_vapid_form() {
    // PR-D3: the component-level disco#info response carries a XEP-0128
    // extension form (FORM_TYPE='urn:waddle:push:vapid:0') with the
    // active VAPID public key + kid so the chat client can subscribe
    // without a build-time embedded key.
    use waddle_xmpp::push::disco::{
        parse_push_vapid_disco_form, PUSH_VAPID_FIELD_KID, PUSH_VAPID_FIELD_PUBLIC_KEY,
        PUSH_VAPID_FORM_TYPE,
    };

    let (_server, mut client) = setup().await;
    let query = Element::builder("query", DISCO_INFO_NS).build();

    let response = send_iq(
        &mut client,
        iq_frame("get", "push-disco-vapid", PUSH_SERVICE_JID, query),
        "push-disco-vapid",
    )
    .await;

    let iq = parse_iq_element(&response, "push-disco-vapid", "result");
    // RFC 6120 §8.1.1.1: the chat's wasm-side `verify_iq_from_matches_query`
    // REQUIRES a present `from` that matches the queried service JID
    // exactly (round-5 tightening — the Push Service is a separate
    // XEP-0114 component, not the user's own server, so §8.1.2.1's
    // absent-from permission does not apply). This assertion pins the
    // server-side stamping so a future regression that drops `from`
    // on the push component's responses is caught immediately, not
    // discovered later via a hostile-C2S spoof CVE.
    assert_eq!(
        iq.attr("from"),
        Some(PUSH_SERVICE_JID),
        "Push Service disco#info result MUST carry from='{PUSH_SERVICE_JID}'"
    );
    let query = single_child(&iq, "query", DISCO_INFO_NS);

    let form = query
        .children()
        .find(|child| {
            child.is("x", waddle_xmpp::xep::NS_DATA_FORMS)
                && xdata_field_value(child, "FORM_TYPE").as_deref() == Some(PUSH_VAPID_FORM_TYPE)
        })
        .unwrap_or_else(|| panic!("VAPID disco form missing from response: {response}"));

    assert_eq!(form.attr("type"), Some("result"), "form is type='result'");

    let public_key =
        xdata_field_value(form, PUSH_VAPID_FIELD_PUBLIC_KEY).expect("public-key field present");
    let kid = xdata_field_value(form, PUSH_VAPID_FIELD_KID).expect("kid field present");
    assert!(!public_key.is_empty(), "public-key value non-empty");
    assert!(!kid.is_empty(), "kid value non-empty");

    // Round-trip: the typed parser must accept the wire form back into
    // a VapidAdvertisement (validates 65-byte SEC1 + 0x04 prefix + valid
    // P-256 point + UUID kid in one shot).
    let advertisement =
        parse_push_vapid_disco_form(form).expect("typed parser accepts the advertised form");
    assert_eq!(advertisement.public_key_base64url(), public_key);
    assert_eq!(advertisement.kid.to_string(), kid);

    let _ = client.close().await;
}

#[tokio::test]
async fn push_service_node_disco_omits_xep0128_vapid_form() {
    // The VAPID form is a *component-level* advertisement; per-node disco
    // returns the leaf-node identity and MUST NOT carry the form (the
    // chat side never asks a leaf node for its parent's VAPID key).
    use waddle_xmpp::push::disco::PUSH_VAPID_FORM_TYPE;

    let (_server, mut client) = setup().await;

    let node = register_web_push_device_via_xep0050(
        &mut client,
        "push-vapid-leaf",
        "web",
        "https://push.example.com/endpoint/vapid-leaf",
        "p256-key-vapid-leaf",
        "auth-secret-vapid-leaf",
    )
    .await;

    let query = Element::builder("query", DISCO_INFO_NS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.as_str())
        .build();
    let response = send_iq(
        &mut client,
        iq_frame("get", "push-vapid-leaf-disco", PUSH_SERVICE_JID, query),
        "push-vapid-leaf-disco",
    )
    .await;

    let iq = parse_iq_element(&response, "push-vapid-leaf-disco", "result");
    let query = single_child(&iq, "query", DISCO_INFO_NS);
    let vapid_form_present = query.children().any(|child| {
        child.is("x", waddle_xmpp::xep::NS_DATA_FORMS)
            && xdata_field_value(child, "FORM_TYPE").as_deref() == Some(PUSH_VAPID_FORM_TYPE)
    });
    assert!(
        !vapid_form_present,
        "leaf-node disco MUST NOT carry the component-level VAPID form: {response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn xep0357_push_service_rejects_client_origin_pubsub_notification_publish() {
    let (_server, mut client) = setup().await;

    let node = register_web_push_device_via_xep0050(
        &mut client,
        "push-cors-reject",
        "web",
        "https://push.example.com/endpoint",
        "provider-key",
        "provider-secret",
    )
    .await;

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

    let notification = Element::builder("notification", NS_PUSH).build();
    let item = Element::builder("item", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "push-1")
        .append(notification)
        .build();
    let publish = Element::builder("publish", NS_PUBSUB)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.as_str())
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
async fn xep0357_offline_dm_emits_durable_summary_pubsub_publish_job() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("xep0357-offline-push.sqlite3");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let server =
        TestServer::start_persistent_with_extra_accounts(&database_url, &[("bob", "bob-password")]);
    let ws_url = server.ws_url();
    let mut bob = WsXmppClient::connect_and_auth(
        &ws_url,
        DOMAIN,
        "bob",
        "bob-password",
        &format!("xep0357-bob-offline-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");

    let node = register_web_push_device_via_xep0050(
        &mut bob,
        "bob-offline-push",
        "web",
        "https://push.example.com/endpoint/bob-offline",
        "bob-p256-key",
        "bob-provider-secret",
    )
    .await;
    let enable = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            PUSH_SERVICE_JID,
        )
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.as_str())
        .build();
    let enable_response = send_iq(
        &mut bob,
        iq_frame("set", "bob-offline-push-enable", DOMAIN, enable),
        "bob-offline-push-enable",
    )
    .await;
    let enable_iq = parse_iq_element(&enable_response, "bob-offline-push-enable", "result");
    assert!(enable_iq.children().next().is_none());
    let _ = bob.close().await;

    let mut admin = WsXmppClient::connect_and_auth(
        &ws_url,
        DOMAIN,
        USERNAME,
        server.fixed_account_password(),
        &format!("xep0357-admin-sender-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("admin connection");
    let offline_message = element_to_xml(
        Element::builder("message", CLIENT_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), "bob@localhost")
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "offline-push-dm-1",
            )
            .append(
                Element::builder("body", CLIENT_NS)
                    .append("durable notification body must stay private")
                    .build(),
            )
            .build(),
    );
    admin.send(&offline_message).await.expect("send offline dm");

    let payload_xml = wait_for_push_publish_payload(&database_url, node.as_str()).await;
    let payload = Element::from_str(&payload_xml).expect("valid notification payload XML");
    assert!(payload.is("notification", NS_PUSH));
    let summary = payload
        .children()
        .find(|child| child.is("x", waddle_xmpp::xep::NS_DATA_FORMS))
        .expect("XEP-0357 summary data form");
    // XEP-0357 §4 example shows `<x xmlns='jabber:x:data'>` with no
    // `type` attribute — XEP-0004 §3.2 reserves `type='result'` for
    // query-response contexts, which doesn't fit a passively-
    // encapsulated summary form.
    assert_eq!(summary.attr("type"), None);
    assert!(summary.children().any(|field| {
        field.is("field", waddle_xmpp::xep::NS_DATA_FORMS)
            && field.attr("var") == Some("FORM_TYPE")
            && field.attr("type") == Some("hidden")
    }));
    assert_eq!(
        xdata_field_value(summary, "FORM_TYPE").as_deref(),
        Some("urn:xmpp:push:summary")
    );
    assert_eq!(
        xdata_field_value(summary, "message-count").as_deref(),
        Some("1")
    );
    let context = payload
        .children()
        .find(|child| child.is("context", NS_WADDLE_PUSH_CONTEXT))
        .expect("Waddle push context");
    assert_eq!(context.attr("conversation"), Some("admin@localhost"));
    assert_eq!(context.attr("class"), Some("dm"));
    assert!(
        !payload_xml.contains("durable notification body"),
        "minimal XEP-0357 payload must not leak the message body"
    );
    assert_eq!(
        notification_candidate_count(&database_url, "bob@localhost").await,
        1
    );

    let _ = admin.close().await;
}

#[tokio::test]
async fn xep0357_first_party_enable_requires_owned_active_node_with_device() {
    let (_server, mut client) = setup().await;

    let enable_unknown_node = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            PUSH_SERVICE_JID,
        )
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            "missing-web-node",
        )
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

    // Under the XEP-0050 cutover, a node only exists AFTER a successful
    // `register-device` round — there is no separate `ensure-node`
    // round-trip. The stage-3 form atomically allocates the node and
    // upserts the device row, so the "enable without a device" case
    // collapses into "enable a node that was never registered" (above).

    let node = register_web_push_device_via_xep0050(
        &mut client,
        "push-enable-register",
        "web",
        "https://push.example.com/endpoint/enable",
        "p256-key-enable",
        "provider-secret-enable",
    )
    .await;

    let enable = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            PUSH_SERVICE_JID,
        )
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.as_str())
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
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            PUSH_SERVICE_JID,
        )
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.as_str())
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
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            PUSH_SERVICE_JID,
        )
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.as_str())
        .append(
            Element::builder("x", waddle_xmpp::xep::NS_DATA_FORMS)
                .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
                .append(
                    Element::builder("field", waddle_xmpp::xep::NS_DATA_FORMS)
                        .attr(minidom::rxml::xml_ncname!("var").to_owned(), "FORM_TYPE")
                        .append(
                            Element::builder("value", waddle_xmpp::xep::NS_DATA_FORMS)
                                .append(waddle_xmpp::xep::NS_PUBSUB_PUBLISH_OPTIONS)
                                .build(),
                        )
                        .build(),
                )
                .append(
                    Element::builder("field", waddle_xmpp::xep::NS_DATA_FORMS)
                        .attr(
                            minidom::rxml::xml_ncname!("var").to_owned(),
                            "unlisted-secret-like-field",
                        )
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

    let bob_node = register_web_push_device_via_xep0050(
        &mut bob,
        "bob-push-foreign",
        "web",
        "https://push.example.com/endpoint/bob-foreign",
        "bob-p256-key",
        "bob-provider-secret",
    )
    .await;

    let admin_enable = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            PUSH_SERVICE_JID,
        )
        .attr(
            minidom::rxml::xml_ncname!("node").to_owned(),
            bob_node.as_str(),
        )
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
async fn xep0050_push_service_register_device_rejects_oversized_app_id() {
    // The XEP-0050 cutover folds the legacy `ensure-node` round-trip
    // into stage-3 of `register-device`. The storage-layer cap on
    // `app_id` length (MAX_APP_ID_LEN = 128) is the same. Submit an
    // oversized app_id and assert the storage validation surfaces a
    // bad-request stanza error through the registry.
    let (_server, mut client) = setup().await;

    // Stage 1 → 2: execute, capture the sessionid.
    let execute = command_element(REGISTER_DEVICE_NODE, "execute", None, None);
    let executing_response = send_iq(
        &mut client,
        iq_frame(
            "set",
            "push-oversized-app-execute",
            PUSH_SERVICE_JID,
            execute,
        ),
        "push-oversized-app-execute",
    )
    .await;
    let executing_iq =
        parse_iq_element(&executing_response, "push-oversized-app-execute", "result");
    let executing_command = single_child(&executing_iq, "command", NS_COMMANDS);
    let session_id = executing_command
        .attr("sessionid")
        .expect("XEP-0050 sessionid")
        .to_string();

    // Stage 3: submit a form with an oversized app_id.
    let oversized_app_id = "x".repeat(129);
    let form = submit_form(
        REGISTER_DEVICE_FORM_TYPE,
        &[
            ("platform", "web"),
            ("environment", "prod"),
            ("app_id", &oversized_app_id),
            ("web-push-endpoint", "https://push.example.com/endpoint/big"),
            ("web-push-p256dh", "p256-key-big"),
            ("web-push-auth", "auth-secret-big"),
        ],
    );
    let complete = command_element(
        REGISTER_DEVICE_NODE,
        "complete",
        Some(&session_id),
        Some(form),
    );
    let response = send_iq(
        &mut client,
        iq_frame(
            "set",
            "push-oversized-app-complete",
            PUSH_SERVICE_JID,
            complete,
        ),
        "push-oversized-app-complete",
    )
    .await;
    assert_iq_error_condition(&response, "push-oversized-app-complete", "bad-request");

    let _ = client.close().await;
}

#[tokio::test]
async fn xep0050_register_device_completes_and_persists_device_row() {
    // End-to-end XEP-0050 contract pin: disco#info advertises the
    // commands feature; disco#items lists the two registered commands;
    // the multi-step `register-device` dance round-trips through to
    // typed storage; `disable-device` retires the node.
    use sqlx::Row;

    let temp_dir = tempfile::tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("xep0050-register-device.sqlite3");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let server = TestServer::start_persistent_with_extra_accounts(&database_url, &[]);
    let password = server.fixed_account_password().to_string();
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("xep0050-register-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("websocket connection");

    // disco#info on push.<domain> must advertise the commands feature.
    let disco_info_query = Element::builder("query", DISCO_INFO_NS).build();
    let info_response = send_iq(
        &mut client,
        iq_frame(
            "get",
            "xep0050-disco-info",
            PUSH_SERVICE_JID,
            disco_info_query,
        ),
        "xep0050-disco-info",
    )
    .await;
    let info_iq = parse_iq_element(&info_response, "xep0050-disco-info", "result");
    let info_query = info_iq
        .children()
        .find(|child| child.is("query", DISCO_INFO_NS))
        .expect("disco#info query child");
    let features = disco_feature_vars(info_query);
    assert!(
        features.contains(NS_COMMANDS),
        "push.<domain> disco#info MUST advertise XEP-0050 commands: {info_response}"
    );

    // disco#items on push.<domain>?node=http://jabber.org/protocol/commands
    // must list the two registered ad-hoc command nodes.
    let commands_items_query = Element::builder("query", DISCO_ITEMS_NS)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), NS_COMMANDS)
        .build();
    let items_response = send_iq(
        &mut client,
        iq_frame(
            "get",
            "xep0050-disco-commands",
            PUSH_SERVICE_JID,
            commands_items_query,
        ),
        "xep0050-disco-commands",
    )
    .await;
    let items_iq = parse_iq_element(&items_response, "xep0050-disco-commands", "result");
    let items_query = single_child(&items_iq, "query", DISCO_ITEMS_NS);
    let item_nodes: std::collections::BTreeSet<String> = items_query
        .children()
        .filter(|child| child.is("item", DISCO_ITEMS_NS))
        .filter_map(|child| child.attr("node").map(str::to_string))
        .collect();
    assert!(
        item_nodes.contains(REGISTER_DEVICE_NODE),
        "disco#items must list register-device: {items_response}"
    );
    assert!(
        item_nodes.contains(DISABLE_DEVICE_NODE),
        "disco#items must list disable-device: {items_response}"
    );

    // Drive the multi-step register-device dance.
    let endpoint = "https://push.example.com/endpoint/xep0050-end-to-end";
    let p256dh = "p256-key-xep0050-end-to-end";
    let auth = "auth-secret-xep0050-end-to-end";
    let outcome = register_web_push_device_via_xep0050_with_device_id(
        &mut client,
        "xep0050-register-flow",
        "web",
        endpoint,
        p256dh,
        auth,
    )
    .await;
    assert!(!outcome.node.is_empty(), "node id must not be empty");
    assert!(!outcome.device_id.is_empty(), "device id must not be empty");

    // Verify the persisted `push_devices` row carries the full set of
    // Web Push provider credentials so a field-shuffle regression in
    // `build_registration` can't silently land garbage. Round-2
    // test-rigor adversarial finding.
    let pool = sqlx::SqlitePool::connect(&database_url)
        .await
        .expect("open sqlite db");
    let row = sqlx::query(
        "SELECT d.platform, d.environment, d.provider_endpoint, d.provider_token, \
                d.provider_key_material, d.device_id, n.app_id, n.owner_bare_jid \
         FROM push_devices d \
         JOIN push_nodes n ON n.node = d.node \
         WHERE d.node = ? AND n.owner_bare_jid = ? AND d.status = 'active' \
         LIMIT 1",
    )
    .bind(&outcome.node)
    .bind(format!("{USERNAME}@{DOMAIN}"))
    .fetch_one(&pool)
    .await
    .expect("active device row exists");
    let platform: String = row.get("platform");
    let environment: String = row.get("environment");
    let provider_endpoint: String = row.get("provider_endpoint");
    let provider_token: String = row.get("provider_token");
    let provider_key_material: String = row.get("provider_key_material");
    let row_device_id: String = row.get("device_id");
    let row_app_id: String = row.get("app_id");
    let row_owner: String = row.get("owner_bare_jid");
    assert_eq!(platform, "web");
    assert_eq!(environment, "prod");
    assert_eq!(row_device_id, outcome.device_id);
    assert_eq!(row_app_id, "web");
    assert_eq!(row_owner, format!("{USERNAME}@{DOMAIN}"));
    // Provider credentials are stored encrypted at rest
    // (`waddle-push-secret-v1:…` envelope) so we can't compare raw
    // plaintexts here. Instead pin that (a) every provider field is
    // present and non-empty AND (b) the three ciphertexts are
    // distinct — a field-shuffle bug in `build_registration` that
    // wrote the same plaintext into two columns would round-trip to
    // identical ciphertexts under the deterministic encryption
    // scheme and fail this assertion. Round-2 test-rigor adversarial
    // finding.
    const ENCRYPTED_ENVELOPE_PREFIX: &str = "waddle-push-secret-v1:";
    for (name, value) in [
        ("provider_endpoint", &provider_endpoint),
        ("provider_token", &provider_token),
        ("provider_key_material", &provider_key_material),
    ] {
        assert!(
            value.starts_with(ENCRYPTED_ENVELOPE_PREFIX),
            "{name} must be stored encrypted: got {value}"
        );
    }
    assert_ne!(provider_endpoint, provider_token);
    assert_ne!(provider_token, provider_key_material);
    assert_ne!(provider_endpoint, provider_key_material);

    // Drive per-device `disable-device` using the device-id the
    // service returned in stage 4. The XEP-0050 cutover scopes
    // disable to a single row — sibling devices on the same node
    // keep receiving fan-out so a per-browser opt-out doesn't take
    // down push for the user's other installs.
    let disable_form = submit_form(
        DISABLE_DEVICE_FORM_TYPE,
        &[
            ("node", outcome.node.as_str()),
            ("device-id", outcome.device_id.as_str()),
        ],
    );
    let disable_cmd = command_element(DISABLE_DEVICE_NODE, "execute", None, Some(disable_form));
    let disable_response = send_iq(
        &mut client,
        iq_frame("set", "xep0050-disable", PUSH_SERVICE_JID, disable_cmd),
        "xep0050-disable",
    )
    .await;
    let disable_iq = parse_iq_element(&disable_response, "xep0050-disable", "result");
    let disable_command = single_child(&disable_iq, "command", NS_COMMANDS);
    assert_eq!(
        disable_command.attr("status"),
        Some("completed"),
        "disable-device must complete in one step: {disable_response}"
    );

    // The targeted device row flips to `disabled`; the node + node
    // record stays alive so a re-register can resurrect it.
    let row_status: String =
        sqlx::query("SELECT status FROM push_devices WHERE node = ? AND device_id = ?")
            .bind(&outcome.node)
            .bind(&outcome.device_id)
            .fetch_one(&pool)
            .await
            .expect("query device row status")
            .get("status");
    assert_eq!(
        row_status, "disabled",
        "disable-device must flip the targeted row to 'disabled'"
    );
    let post_disable: i64 = sqlx::query(
        "SELECT COUNT(*) AS count \
         FROM push_devices \
         WHERE node = ? AND status = 'active'",
    )
    .bind(&outcome.node)
    .fetch_one(&pool)
    .await
    .expect("query active devices count")
    .get("count");
    // After the per-device disable, the single web-push row is the
    // only device on the node, so the active count drops to 0.
    // Multi-device coverage (a second sibling device on the same
    // node that stays active after the targeted disable) lives in
    // the lib-level unit test
    // `handle_iq_push_service_xep0050_disable_device_is_per_device_scoped`
    // — keeping that probe out of the WS layer avoids spinning up a
    // second authenticated WS client just to register a sibling.
    assert_eq!(
        post_disable, 0,
        "disable-device flipped the only device on the node to 'disabled'"
    );

    let _ = client.close().await;
}
