//! Integration tests for the `urn:waddle:threads:0` IQ over the
//! WebSocket transport.
//!
//! Spec: `docs/superpowers/specs/2026-05-17-threads-design.md`.
//! Plan: `docs/superpowers/plans/2026-05-17-threads-implementation.md` Task 5.

mod ws_common;

use std::str::FromStr;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const CLIENT_NS: &str = "jabber:client";
const NS_THREADS: &str = "urn:waddle:threads:0";
const NS_RSM: &str = "http://jabber.org/protocol/rsm";
const STANZA_ERROR_NS: &str = "urn:ietf:params:xml:ns:xmpp-stanzas";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn admin_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, USERNAME, &password, resource)
        .await
        .expect("connect")
}

async fn extra_client(
    server: &TestServer,
    username: &str,
    password: &str,
    resource: &str,
) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, username, password, resource)
        .await
        .expect("connect extra")
}

fn threads_query_iq(id: &str, page_size: Option<u32>, after: Option<&str>) -> String {
    let mut query = Element::builder("query", NS_THREADS).build();
    if page_size.is_some() || after.is_some() {
        let mut set = Element::builder("set", NS_RSM).build();
        if let Some(max) = page_size {
            let mut max_el = Element::builder("max", NS_RSM).build();
            max_el.append_text_node(max.to_string());
            set.append_child(max_el);
        }
        if let Some(cursor) = after {
            let mut after_el = Element::builder("after", NS_RSM).build();
            after_el.append_text_node(cursor);
            set.append_child(after_el);
        }
        query.append_child(set);
    }
    let iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(query)
        .build();
    element_to_xml(iq)
}

fn threads_query_iq_to(id: &str, to: &str) -> String {
    let query = Element::builder("query", NS_THREADS).build();
    let iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), to)
        .append(query)
        .build();
    element_to_xml(iq)
}

fn element_to_xml(element: Element) -> String {
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize XML");
    String::from_utf8(buf).expect("UTF-8")
}

async fn send_iq(client: &mut WsXmppClient, frame: String, id: &str) -> String {
    client.send(&frame).await.expect("send iq");
    client
        .recv_matching(|candidate| candidate.contains(id) && candidate.contains("<iq"))
        .await
        .expect("iq response")
}

async fn join_room(client: &mut WsXmppClient, room: &str) {
    client
        .send(&format!(
            r#"<presence to="{room}/{USERNAME}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
}

fn parse_iq_element(xml: &str, id: &str, iq_type: &str) -> Element {
    let element = Element::from_str(xml).expect("valid XML response");
    assert_eq!(element.name(), "iq");
    assert_eq!(element.attr("id"), Some(id));
    assert_eq!(
        element.attr("type"),
        Some(iq_type),
        "unexpected iq type in response: {xml}"
    );
    element
}

fn assert_threads_attrs(element: &Element, total: u64, unread_threads: u64) {
    let threads = element
        .children()
        .find(|child| child.name() == "threads" && child.ns() == NS_THREADS)
        .expect("threads element");
    assert_eq!(
        threads.attr("total"),
        Some(total.to_string().as_str()),
        "unexpected total: {element:?}"
    );
    assert_eq!(
        threads.attr("unread-threads"),
        Some(unread_threads.to_string().as_str()),
        "unexpected unread-threads: {element:?}"
    );
}

fn thread_children(element: &Element) -> Vec<&Element> {
    element
        .children()
        .find(|c| c.name() == "threads" && c.ns() == NS_THREADS)
        .map(|threads| {
            threads
                .children()
                .filter(|c| c.name() == "thread" && c.ns() == NS_THREADS)
                .collect()
        })
        .unwrap_or_default()
}

fn assert_error_condition(xml: &str, condition: &str) {
    let element = Element::from_str(xml).expect("valid XML");
    assert_eq!(element.attr("type"), Some("error"), "expected error: {xml}");
    let error = element
        .children()
        .find(|child| child.name() == "error")
        .expect("error element");
    assert!(
        error
            .children()
            .any(|child| child.name() == condition && child.ns() == STANZA_ERROR_NS),
        "expected <{condition}/> in error: {xml}"
    );
}

/// Drive a single MUC message with a thread anchor to make the server
/// project a thread row into the bound user's inbox.
async fn send_threaded_message(
    client: &mut WsXmppClient,
    room: &str,
    thread_id: &str,
    body: &str,
    msg_id: &str,
) {
    client
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="{msg_id}"><body>{body}</body><thread>{thread_id}</thread></message>"#
        ))
        .await
        .expect("send threaded message");
    // Wait for the echo so the projection has surely run.
    client
        .recv_matching(|frame| frame.contains(body) && frame.contains(thread_id))
        .await
        .expect("echoed threaded message");
}

#[tokio::test]
async fn fresh_account_returns_empty_page() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut client = admin_client(&server, "th-empty").await;

    let resp = send_iq(
        &mut client,
        threads_query_iq("th-empty-1", None, None),
        "th-empty-1",
    )
    .await;
    let iq = parse_iq_element(&resp, "th-empty-1", "result");
    assert_threads_attrs(&iq, 0, 0);
    assert!(thread_children(&iq).is_empty());

    let _ = client.close().await;
}

#[tokio::test]
async fn populated_inbox_returns_thread_entries() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut client = admin_client(&server, "th-pop").await;
    let room = format!("threads-pop-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    send_threaded_message(&mut client, &room, "t-one", "first reply", "tm-1").await;
    send_threaded_message(&mut client, &room, "t-one", "second reply", "tm-2").await;
    send_threaded_message(&mut client, &room, "t-two", "another thread", "tm-3").await;

    // Give the inbox projection a beat to settle.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let resp = send_iq(
        &mut client,
        threads_query_iq("th-pop-1", None, None),
        "th-pop-1",
    )
    .await;
    let iq = parse_iq_element(&resp, "th-pop-1", "result");
    let entries = thread_children(&iq);
    assert!(
        entries.len() >= 2,
        "expected at least 2 thread entries, got {}: {iq:?}",
        entries.len()
    );
    let thread_ids: Vec<&str> = entries
        .iter()
        .filter_map(|el| el.attr("thread-id"))
        .collect();
    assert!(
        thread_ids.contains(&"t-one"),
        "missing thread t-one: {thread_ids:?}"
    );
    assert!(
        thread_ids.contains(&"t-two"),
        "missing thread t-two: {thread_ids:?}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn pagination_round_trips_cursor() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut client = admin_client(&server, "th-page").await;
    let room = format!("threads-page-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    for (idx, label) in ["alpha", "bravo", "charlie"].iter().enumerate() {
        send_threaded_message(
            &mut client,
            &room,
            &format!("t-{label}"),
            &format!("entry {label}"),
            &format!("tm-page-{idx}"),
        )
        .await;
    }
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let first = send_iq(
        &mut client,
        threads_query_iq("th-page-1", Some(2), None),
        "th-page-1",
    )
    .await;
    let first_iq = parse_iq_element(&first, "th-page-1", "result");
    let first_entries = thread_children(&first_iq);
    assert_eq!(
        first_entries.len(),
        2,
        "page should return 2 entries: {first_iq:?}"
    );

    let threads_el = first_iq
        .children()
        .find(|c| c.name() == "threads" && c.ns() == NS_THREADS)
        .expect("threads");
    let rsm_set = threads_el
        .children()
        .find(|c| c.name() == "set" && c.ns() == NS_RSM)
        .expect("rsm set");
    let last_cursor = rsm_set
        .children()
        .find(|c| c.name() == "last" && c.ns() == NS_RSM)
        .map(|el| el.text())
        .expect("last cursor");
    assert!(!last_cursor.is_empty(), "last cursor must be non-empty");

    let second = send_iq(
        &mut client,
        threads_query_iq("th-page-2", Some(2), Some(&last_cursor)),
        "th-page-2",
    )
    .await;
    let second_iq = parse_iq_element(&second, "th-page-2", "result");
    let second_entries = thread_children(&second_iq);
    assert_eq!(
        second_entries.len(),
        1,
        "second page should return 1 entry: {second_iq:?}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn acl_refuses_cross_user_query() {
    let _guard = TEST_SERIAL.lock().await;
    let bob_password = format!("ws-test-bob-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[("bob", bob_password.as_str())]);
    let mut bob = extra_client(&server, "bob", &bob_password, "th-acl-bob").await;

    let resp = send_iq(
        &mut bob,
        threads_query_iq_to("th-acl-1", &format!("{USERNAME}@{DOMAIN}")),
        "th-acl-1",
    )
    .await;
    assert_error_condition(&resp, "forbidden");

    let _ = bob.close().await;
}

#[tokio::test]
async fn has_unread_attribute_present_on_each_entry() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut client = admin_client(&server, "th-hasunread").await;
    let room = format!("threads-hu-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut client, &room).await;

    send_threaded_message(&mut client, &room, "t-x", "hello", "tm-x").await;
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    let resp = send_iq(
        &mut client,
        threads_query_iq("th-hu-1", None, None),
        "th-hu-1",
    )
    .await;
    let iq = parse_iq_element(&resp, "th-hu-1", "result");
    let entries = thread_children(&iq);
    assert!(
        !entries.is_empty(),
        "expected at least one thread entry: {iq:?}"
    );
    for entry in entries {
        let has_unread = entry.attr("has-unread");
        assert!(
            matches!(has_unread, Some("true") | Some("false")),
            "<thread> must have explicit has-unread attribute: {entry:?}"
        );
    }

    let _ = client.close().await;
}

#[tokio::test]
async fn disco_info_on_self_advertises_threads_query_feature() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut client = admin_client(&server, "th-disco").await;
    let self_bare = format!("{USERNAME}@{DOMAIN}");

    let disco_payload = Element::builder("query", "http://jabber.org/protocol/disco#info").build();
    let iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), "th-disco-1")
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), &self_bare)
        .append(disco_payload)
        .build();
    let resp = send_iq(&mut client, element_to_xml(iq), "th-disco-1").await;
    let iq = parse_iq_element(&resp, "th-disco-1", "result");
    let query = iq
        .children()
        .find(|c| c.name() == "query" && c.ns() == "http://jabber.org/protocol/disco#info")
        .expect("disco#info query");
    let features: Vec<&str> = query
        .children()
        .filter(|c| c.name() == "feature")
        .filter_map(|c| c.attr("var"))
        .collect();
    assert!(
        features.contains(&"urn:waddle:threads:0"),
        "disco#info on self must advertise urn:waddle:threads:0: features={features:?}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn response_includes_rsm_set_with_count() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start();
    let mut client = admin_client(&server, "th-rsm").await;

    let resp = send_iq(
        &mut client,
        threads_query_iq("th-rsm-1", None, None),
        "th-rsm-1",
    )
    .await;
    let iq = parse_iq_element(&resp, "th-rsm-1", "result");
    let threads = iq
        .children()
        .find(|c| c.name() == "threads" && c.ns() == NS_THREADS)
        .expect("threads element");
    let rsm = threads
        .children()
        .find(|c| c.name() == "set" && c.ns() == NS_RSM)
        .expect("rsm set");
    let count = rsm
        .children()
        .find(|c| c.name() == "count" && c.ns() == NS_RSM)
        .map(|el| el.text())
        .expect("rsm count");
    assert_eq!(count, "0");

    let _ = client.close().await;
}
