//! Integration tests for issue #367 — `urn:waddle:dnd:0` PEP publish,
//! projection write, and T1 push gating.
//!
//! Three scenarios:
//!
//! 1. **Publish round-trip + projection write** — Bob publishes
//!    `<dnd>` with a snooze in the future; the `dnd_projection` row
//!    appears and the payload XML matches the published shape.
//!
//! 2. **Invalid payload bad-request** — Bob publishes a malformed
//!    `<dnd>` (unknown timezone); the wire response is
//!    `<bad-request/>` and no projection row is written.
//!
//! 3. **End-to-end push suppression** — Bob registers + enables push,
//!    publishes a snooze, disconnects, admin sends an offline DM;
//!    the T1 drain marks the candidate `suppressed_reason='waddle_dnd'`
//!    and no `push_publish_jobs` row appears. The janitor tick is
//!    accelerated via `WADDLE_NOTIFICATION_OUTBOX_JANITOR_INTERVAL=1`
//!    so the test stays within a reasonable wall-clock budget.

mod ws_common;

use std::{
    str::FromStr,
    time::{Duration, Instant},
};

use sqlx::Row;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const CLIENT_NS: &str = "jabber:client";
const NS_COMMANDS: &str = "http://jabber.org/protocol/commands";
const NS_DATA_FORMS: &str = "jabber:x:data";
const NS_PUBSUB: &str = "http://jabber.org/protocol/pubsub";
const NS_PUSH: &str = "urn:xmpp:push:0";
const NS_WADDLE_DND: &str = "urn:waddle:dnd:0";
const REGISTER_DEVICE_NODE: &str = "register-device";
const REGISTER_DEVICE_FORM_TYPE: &str = "urn:waddle:push-service:commands:register-device:0";
const STANZA_ERROR_NS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";

const DOMAIN: &str = "localhost";
const ADMIN: &str = "admin";
const PUSH_SERVICE_JID: &str = "push.localhost";

fn element_to_xml(element: Element) -> String {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize element");
    String::from_utf8(bytes).expect("serializer emits utf-8")
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

fn parse_iq(xml: &str, id: &str, iq_type: &str) -> Element {
    let element = Element::from_str(xml).expect("valid XML iq");
    assert_eq!(element.name(), "iq");
    assert_eq!(element.attr("id"), Some(id));
    assert_eq!(element.attr("type"), Some(iq_type));
    element
}

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

fn xdata_field_value_in(parent: &Element, var: &str) -> Option<String> {
    let form = parent
        .children()
        .find(|child| child.is("x", NS_DATA_FORMS))?;
    form.children()
        .find(|child| child.is("field", NS_DATA_FORMS) && child.attr("var") == Some(var))
        .and_then(|field| {
            field
                .children()
                .find(|child| child.is("value", NS_DATA_FORMS))
        })
        .map(|value| value.text())
}

async fn register_web_push_device_via_xep0050(
    client: &mut WsXmppClient,
    id_prefix: &str,
    app_id: &str,
    endpoint: &str,
    p256dh: &str,
    auth: &str,
) -> String {
    let execute_id = format!("{id_prefix}-execute");
    let execute = command_element(REGISTER_DEVICE_NODE, "execute", None, None);
    let executing_response = send_iq(
        client,
        iq_frame("set", &execute_id, PUSH_SERVICE_JID, execute),
        &execute_id,
    )
    .await;
    let executing_iq = Element::from_str(&executing_response).expect("executing iq");
    let executing_command = executing_iq
        .children()
        .find(|child| child.is("command", NS_COMMANDS))
        .expect("command child in executing response");
    let session_id = executing_command
        .attr("sessionid")
        .expect("XEP-0050 sessionid")
        .to_string();

    let complete_id = format!("{id_prefix}-complete");
    let form = submit_form(
        REGISTER_DEVICE_FORM_TYPE,
        &[
            ("platform", "web"),
            ("environment", "prod"),
            ("app-id", app_id),
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
    let completed_iq = Element::from_str(&completed_response).expect("completed iq");
    let completed_command = completed_iq
        .children()
        .find(|child| child.is("command", NS_COMMANDS))
        .expect("command child in completed response");
    xdata_field_value_in(completed_command, "node").expect("stage 4 result form carries `node`")
}

const NS_PUBSUB_ERRORS: &str = "http://jabber.org/protocol/pubsub#errors";

fn assert_bad_request(xml: &str, id: &str) {
    let iq = parse_iq(xml, id, "error");
    let error = iq
        .children()
        .find(|c| c.name() == "error")
        .expect("error child");
    assert!(
        error
            .children()
            .any(|c| c.name() == "bad-request" && c.ns() == STANZA_ERROR_NS),
        "expected <bad-request/> stanza error: {xml}"
    );
    // XEP-0060 §7.1.3.4: payload-shape rejection on a PubSub publish
    // MUST also carry the pubsub-namespaced `<invalid-payload/>`
    // extension so clients can distinguish payload-shape errors from
    // generic bad-request.
    assert!(
        error
            .children()
            .any(|c| c.name() == "invalid-payload" && c.ns() == NS_PUBSUB_ERRORS),
        "expected XEP-0060 <invalid-payload/> pubsub-errors extension: {xml}"
    );
}

fn build_dnd_payload(timezone: &str, snooze_until: Option<&str>) -> Element {
    let mut builder = Element::builder("dnd", NS_WADDLE_DND)
        .attr(minidom::rxml::xml_ncname!("timezone").to_owned(), timezone);
    if let Some(until) = snooze_until {
        builder = builder.append(
            Element::builder("snooze", NS_WADDLE_DND)
                .attr(minidom::rxml::xml_ncname!("until").to_owned(), until),
        );
    }
    builder.build()
}

fn build_publish_dnd(item_id: &str, payload: Element) -> Element {
    Element::builder("pubsub", NS_PUBSUB)
        .append(
            Element::builder("publish", NS_PUBSUB)
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), NS_WADDLE_DND)
                .append(
                    Element::builder("item", NS_PUBSUB)
                        .attr(minidom::rxml::xml_ncname!("id").to_owned(), item_id)
                        .append(payload)
                        .build(),
                )
                .build(),
        )
        .build()
}

async fn open_test_pool(database_url: &str) -> sqlx::SqlitePool {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
    let options = SqliteConnectOptions::from_str(database_url)
        .expect("parse sqlite url")
        .busy_timeout(Duration::from_secs(5));
    SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("open sqlite db with busy_timeout")
}

async fn wait_for_dnd_projection_row(database_url: &str, owner: &str) -> String {
    let pool = open_test_pool(database_url).await;
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if let Some(row) =
            sqlx::query("SELECT payload_xml FROM dnd_projection WHERE owner_bare_jid = ?")
                .bind(owner)
                .fetch_optional(&pool)
                .await
                .expect("query dnd_projection")
        {
            return row.get("payload_xml");
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for dnd_projection row for {owner}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

async fn wait_for_suppressed_reason(database_url: &str, recipient: &str, reason: &str) {
    let pool = open_test_pool(database_url).await;
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let row = sqlx::query(
            "SELECT suppressed_reason FROM notification_candidates \
             WHERE recipient_bare_jid = ? ORDER BY created_at_ms DESC LIMIT 1",
        )
        .bind(recipient)
        .fetch_optional(&pool)
        .await
        .expect("query notification_candidates");
        if let Some(row) = row {
            let value: Option<String> = row.get("suppressed_reason");
            if value.as_deref() == Some(reason) {
                return;
            }
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for notification_candidates.suppressed_reason='{reason}' for {recipient}"
        );
        tokio::time::sleep(Duration::from_millis(250)).await;
    }
}

async fn count_push_publish_jobs(database_url: &str, owner: &str) -> i64 {
    let pool = open_test_pool(database_url).await;
    let row =
        sqlx::query("SELECT COUNT(*) AS count FROM push_publish_jobs WHERE owner_bare_jid = ?")
            .bind(owner)
            .fetch_one(&pool)
            .await
            .expect("count push_publish_jobs");
    row.get("count")
}

async fn count_dnd_projection(database_url: &str, owner: &str) -> i64 {
    let pool = open_test_pool(database_url).await;
    let row = sqlx::query("SELECT COUNT(*) AS count FROM dnd_projection WHERE owner_bare_jid = ?")
        .bind(owner)
        .fetch_one(&pool)
        .await
        .expect("count dnd_projection");
    row.get("count")
}

/// Bob publishes `<dnd xmlns='urn:waddle:dnd:0' timezone='Europe/Oslo'>
/// <snooze until='…'/></dnd>` to his own PEP service. The publish-hook
/// in `pubsub/item.rs` MUST validate the payload AND write a
/// `dnd_projection` row in the same transaction as the `pubsub_items`
/// row. We assert via SQL that the projection row appears and its
/// payload XML round-trips the published shape.
#[tokio::test]
async fn dnd_pep_publish_writes_projection_row() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("dnd-publish.sqlite3");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let server = TestServer::start_persistent_with_extra_envs(
        &database_url,
        &[("bob", "bob-dnd-password")],
        &[("WADDLE_XMPP_PUBSUB_DATABASE_URL", &database_url)],
    );
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        "bob-dnd-password",
        &format!("dnd-bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");

    let publish = build_publish_dnd(
        "current",
        build_dnd_payload("Europe/Oslo", Some("2099-01-01T17:00:00Z")),
    );
    let response = send_iq(
        &mut bob,
        iq_frame("set", "dnd-publish", "bob@localhost", publish),
        "dnd-publish",
    )
    .await;
    let iq = parse_iq(&response, "dnd-publish", "result");
    assert_eq!(iq.attr("from"), Some("bob@localhost"));

    let payload_xml = wait_for_dnd_projection_row(&database_url, "bob@localhost").await;
    // Payload XML is the serialised `<dnd>` element produced by the
    // typed Element::builder path. Decode and assert structurally
    // (attribute ordering may differ; element identity must match).
    let stored: Element = payload_xml.parse().expect("stored dnd payload xml parses");
    assert_eq!(stored.name(), "dnd");
    assert_eq!(stored.ns(), NS_WADDLE_DND);
    assert_eq!(stored.attr("timezone"), Some("Europe/Oslo"));
    let snooze = stored
        .children()
        .find(|c| c.name() == "snooze" && c.ns() == NS_WADDLE_DND)
        .expect("stored snooze child");
    assert_eq!(snooze.attr("until"), Some("2099-01-01T17:00:00+00:00"));

    let _ = bob.close().await;
}

/// Malformed DND publishes (here: a `timezone='…'` value that does not
/// resolve via `chrono-tz`) MUST be rejected with `<bad-request/>` at
/// the publish boundary so the projection cannot be poisoned with
/// unparseable XML. The projection row MUST NOT be written.
#[tokio::test]
async fn dnd_pep_publish_invalid_payload_returns_bad_request() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("dnd-bad-request.sqlite3");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let server = TestServer::start_persistent_with_extra_envs(
        &database_url,
        &[("bob", "bob-dnd-password")],
        &[("WADDLE_XMPP_PUBSUB_DATABASE_URL", &database_url)],
    );
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        "bob-dnd-password",
        &format!("dnd-bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");

    let publish = build_publish_dnd(
        "current",
        build_dnd_payload("Not/A/Real_Zone", Some("2099-01-01T17:00:00Z")),
    );
    let response = send_iq(
        &mut bob,
        iq_frame("set", "dnd-bad", "bob@localhost", publish),
        "dnd-bad",
    )
    .await;
    assert_bad_request(&response, "dnd-bad");

    // Projection row MUST NOT exist for invalid publishes — the
    // validate-up-front gate in `pubsub/item.rs` rejects before the
    // transaction commits.
    let count = count_dnd_projection(&database_url, "bob@localhost").await;
    assert_eq!(
        count, 0,
        "invalid DND publish must not write a projection row"
    );

    let _ = bob.close().await;
}

/// Bob's own resource MUST be able to read his published DND state
/// via a XEP-0060 `<items/>` get even though the DND PEP node is
/// configured `access_model = whitelist` + `send_last_published_item
/// = never`. The "never" config blocks the automatic push-on-subscribe
/// fanout (which would leak the user's schedule to roster contacts);
/// it must NOT block the user's own resource from fetching state on
/// resume. The owner ↔ entity affiliation derivation in
/// `pubsub_authz::effective_affiliation` grants Bob `Owner` on his
/// own PEP node, so the items-get path resolves.
#[tokio::test]
async fn dnd_owner_resource_can_fetch_via_items_get_despite_whitelist() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("dnd-items-get.sqlite3");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let server = TestServer::start_persistent_with_extra_envs(
        &database_url,
        &[("bob", "bob-dnd-password")],
        &[("WADDLE_XMPP_PUBSUB_DATABASE_URL", &database_url)],
    );
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        "bob-dnd-password",
        &format!("dnd-items-bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");

    let publish = build_publish_dnd(
        "current",
        build_dnd_payload("Europe/Oslo", Some("2099-01-01T17:00:00Z")),
    );
    let publish_response = send_iq(
        &mut bob,
        iq_frame("set", "dnd-items-publish", "bob@localhost", publish),
        "dnd-items-publish",
    )
    .await;
    parse_iq(&publish_response, "dnd-items-publish", "result");

    let items_request = Element::builder("pubsub", NS_PUBSUB)
        .append(
            Element::builder("items", NS_PUBSUB)
                .attr(minidom::rxml::xml_ncname!("node").to_owned(), NS_WADDLE_DND)
                .build(),
        )
        .build();
    let items_response = send_iq(
        &mut bob,
        iq_frame("get", "dnd-items-get", "bob@localhost", items_request),
        "dnd-items-get",
    )
    .await;
    let iq = parse_iq(&items_response, "dnd-items-get", "result");
    let pubsub = iq
        .children()
        .find(|c| c.name() == "pubsub")
        .expect("pubsub child in items result");
    let items = pubsub
        .children()
        .find(|c| c.name() == "items")
        .expect("items child");
    let item = items
        .children()
        .find(|c| c.name() == "item")
        .expect("DND item visible to owner");
    assert_eq!(item.attr("id"), Some("current"));
    let dnd = item
        .children()
        .find(|c| c.name() == "dnd" && c.ns() == NS_WADDLE_DND)
        .expect("<dnd> payload child");
    assert_eq!(dnd.attr("timezone"), Some("Europe/Oslo"));

    let _ = bob.close().await;
}

/// The `dnd_projection` row MUST survive a server restart pointed at
/// the same SQLite file. Without durability the second server would
/// treat Bob as not-in-DND on its first push gate run, defeating the
/// whole projection model.
#[tokio::test]
async fn dnd_projection_survives_server_restart() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("dnd-restart.sqlite3");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());

    // ── Phase 1: publish DND, verify projection exists. ──────────
    {
        let server = TestServer::start_persistent_with_extra_envs(
            &database_url,
            &[("bob", "bob-dnd-restart-password")],
            &[("WADDLE_XMPP_PUBSUB_DATABASE_URL", &database_url)],
        );
        let mut bob = WsXmppClient::connect_and_auth(
            &server.ws_url(),
            DOMAIN,
            "bob",
            "bob-dnd-restart-password",
            &format!("dnd-restart-bob-{}", uuid::Uuid::new_v4()),
        )
        .await
        .expect("bob connection (phase 1)");

        let publish = build_publish_dnd(
            "current",
            build_dnd_payload("Europe/Oslo", Some("2099-01-01T17:00:00Z")),
        );
        let response = send_iq(
            &mut bob,
            iq_frame("set", "dnd-restart-publish", "bob@localhost", publish),
            "dnd-restart-publish",
        )
        .await;
        parse_iq(&response, "dnd-restart-publish", "result");
        let payload_xml = wait_for_dnd_projection_row(&database_url, "bob@localhost").await;
        assert!(
            payload_xml.contains("Europe/Oslo"),
            "phase 1 projection must store Oslo timezone, got: {payload_xml}"
        );

        let _ = bob.close().await;
        // First TestServer drops here, killing the binary.
    }

    // ── Phase 2: new server, projection row + payload survive. ───
    {
        let _server = TestServer::start_persistent_with_extra_envs(
            &database_url,
            &[("bob", "bob-dnd-restart-password")],
            &[("WADDLE_XMPP_PUBSUB_DATABASE_URL", &database_url)],
        );
        let pool = open_test_pool(&database_url).await;
        let row = sqlx::query("SELECT payload_xml FROM dnd_projection WHERE owner_bare_jid = ?")
            .bind("bob@localhost")
            .fetch_optional(&pool)
            .await
            .expect("query dnd_projection after restart");
        let row = row.expect("projection row survives restart");
        let payload_xml: String = row.get("payload_xml");
        let stored: Element = payload_xml.parse().expect("payload xml parses");
        assert_eq!(stored.attr("timezone"), Some("Europe/Oslo"));
        let snooze = stored
            .children()
            .find(|c| c.name() == "snooze" && c.ns() == NS_WADDLE_DND)
            .expect("snooze child survives restart");
        assert_eq!(snooze.attr("until"), Some("2099-01-01T17:00:00+00:00"));
    }
}

/// End-to-end T1 push suppression. Bob registers + enables push, then
/// publishes a snooze valid through the year 2099. After Bob
/// disconnects, admin sends Bob a DM. The T1 push gate MUST mark the
/// resulting `notification_candidates` row
/// `suppressed_reason='waddle_dnd'` and skip the
/// `push_publish_jobs` insert that would otherwise dispatch to APNs/FCM.
///
/// The janitor tick is accelerated to 1 s so the test stays within a
/// reasonable wall-clock budget (drain interval default is 5 s).
#[tokio::test]
async fn dnd_active_suppresses_offline_dm_push_at_t1() {
    let temp_dir = tempfile::tempdir().expect("temp dir");
    let db_path = temp_dir.path().join("dnd-push-suppress.sqlite3");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let server = TestServer::start_persistent_with_extra_envs(
        &database_url,
        &[("bob", "bob-dnd-push-password")],
        &[
            ("WADDLE_NOTIFICATION_OUTBOX_JANITOR_INTERVAL", "1"),
            ("WADDLE_XMPP_PUBSUB_DATABASE_URL", &database_url),
        ],
    );
    let ws_url = server.ws_url();
    let admin_password = server.fixed_account_password().to_string();
    let mut bob = WsXmppClient::connect_and_auth(
        &ws_url,
        DOMAIN,
        "bob",
        "bob-dnd-push-password",
        &format!("dnd-push-bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");

    // Bob registers push device + enables via XEP-0050.
    let node = register_web_push_device_via_xep0050(
        &mut bob,
        "dnd-bob-push",
        "web",
        "https://push.example.com/endpoint/dnd-bob",
        "bob-dnd-p256-key",
        "bob-dnd-provider-secret",
    )
    .await;
    let enable = Element::builder("enable", NS_PUSH)
        .attr(
            minidom::rxml::xml_ncname!("jid").to_owned(),
            PUSH_SERVICE_JID,
        )
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node.as_str())
        .build();
    let _ = send_iq(
        &mut bob,
        iq_frame("set", "dnd-bob-enable", DOMAIN, enable),
        "dnd-bob-enable",
    )
    .await;

    // Bob publishes DND with a snooze well into the future.
    let publish = build_publish_dnd(
        "current",
        build_dnd_payload("UTC", Some("2099-01-01T17:00:00Z")),
    );
    let publish_response = send_iq(
        &mut bob,
        iq_frame("set", "dnd-bob-publish", "bob@localhost", publish),
        "dnd-bob-publish",
    )
    .await;
    parse_iq(&publish_response, "dnd-bob-publish", "result");
    wait_for_dnd_projection_row(&database_url, "bob@localhost").await;

    let _ = bob.close().await;

    // Admin sends Bob an offline DM. T0 emits a candidate; T1
    // (1 s janitor tick) MUST classify it `waddle_dnd` and NOT
    // dispatch a push job.
    let mut admin = WsXmppClient::connect_and_auth(
        &ws_url,
        DOMAIN,
        ADMIN,
        &admin_password,
        &format!("dnd-admin-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("admin connection");
    let offline_message = element_to_xml(
        Element::builder("message", CLIENT_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "chat")
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), "bob@localhost")
            .attr(
                minidom::rxml::xml_ncname!("id").to_owned(),
                "dnd-offline-dm",
            )
            .append(
                Element::builder("body", CLIENT_NS)
                    .append("DND offline DM must be suppressed at the push gate")
                    .build(),
            )
            .build(),
    );
    admin.send(&offline_message).await.expect("send offline dm");

    wait_for_suppressed_reason(&database_url, "bob@localhost", "waddle_dnd").await;

    // No `push_publish_jobs` row should have landed for Bob during
    // the suppression window.
    let push_jobs = count_push_publish_jobs(&database_url, "bob@localhost").await;
    assert_eq!(
        push_jobs, 0,
        "DND-suppressed candidate must not dispatch to push_publish_jobs"
    );

    let _ = admin.close().await;
}
