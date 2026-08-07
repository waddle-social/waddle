//! Stateful legacy C2S IQs over the active WebSocket transport.

use waddle_ws_test_support as ws_common;

use std::time::Duration;
use ws_common::{disco_info_query, TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const CLIENT_NS: &str = "jabber:client";
const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";
const NS_PUBSUB_PUBLISH_OPTIONS: &str = "http://jabber.org/protocol/pubsub#publish-options";
const NS_PUSH: &str = "urn:xmpp:push:0";
const NS_XDATA: &str = "jabber:x:data";
const NS_PRIVATE: &str = "jabber:iq:private";
const NS_TEST_PRIVATE: &str = "urn:waddle:test";

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();
    let client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("stateful-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("websocket connection");
    (server, client)
}

async fn setup_with_database_url(database_url: &str) -> (TestServer, WsXmppClient) {
    let server = TestServer::start_persistent_with_extra_accounts(database_url, &[]);
    let password = server.fixed_account_password().to_string();
    let client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("stateful-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("websocket connection");
    (server, client)
}

async fn assert_no_frame_matching<F>(
    client: &mut WsXmppClient,
    duration: Duration,
    predicate: F,
    description: &str,
) where
    F: Fn(&str) -> bool,
{
    let deadline = tokio::time::Instant::now() + duration;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return;
        }
        let Ok(frame) = client.recv_timeout(remaining).await else {
            return;
        };
        assert!(!predicate(&frame), "{description}: {frame}");
    }
}

async fn connect_alice_bob() -> (TestServer, WsXmppClient, WsXmppClient) {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);

    let alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("alice-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("alice connection");
    let bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");

    (server, alice, bob)
}

async fn send_roster_get(client: &mut WsXmppClient, id: &str) {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="{id}"><query xmlns="jabber:iq:roster"/></iq>"#
        ))
        .await
        .expect("send roster get");
    let _ = client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("roster get result");
}

fn element_to_xml(element: Element) -> String {
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize XML element");
    String::from_utf8(buf).expect("minidom serializes UTF-8")
}

fn private_storage_iq(
    id: &str,
    iq_type: &str,
    to: Option<&str>,
    children: impl IntoIterator<Item = Element>,
) -> String {
    let mut query = Element::builder("query", NS_PRIVATE);
    for child in children {
        query = query.append(child);
    }

    let mut iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), iq_type)
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id);
    if let Some(to) = to {
        iq = iq.attr(minidom::rxml::xml_ncname!("to").to_owned(), to);
    }

    element_to_xml(iq.append(query.build()).build())
}

fn push_enable_iq(id: &str, jid: &str, node: &str, publish_options: Option<Element>) -> String {
    let mut enable = Element::builder("enable", NS_PUSH)
        .attr(minidom::rxml::xml_ncname!("jid").to_owned(), jid)
        .attr(minidom::rxml::xml_ncname!("node").to_owned(), node);
    if let Some(publish_options) = publish_options {
        enable = enable.append(publish_options);
    }

    element_to_xml(
        Element::builder("iq", CLIENT_NS)
            .attr(minidom::rxml::xml_ncname!("type").to_owned(), "set")
            .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
            .append(enable.build())
            .build(),
    )
}

fn xdata_field(var: &str, value: &str) -> Element {
    Element::builder("field", NS_XDATA)
        .attr(minidom::rxml::xml_ncname!("var").to_owned(), var)
        .append(Element::builder("value", NS_XDATA).append(value).build())
        .build()
}

fn publish_options_form(fields: impl IntoIterator<Item = Element>) -> Element {
    let mut form = Element::builder("x", NS_XDATA)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "submit")
        .append(xdata_field("FORM_TYPE", NS_PUBSUB_PUBLISH_OPTIONS));
    for field in fields {
        form = form.append(field);
    }
    form.build()
}

async fn establish_subscription_to_alice(alice: &mut WsXmppClient, bob: &mut WsXmppClient) {
    send_roster_get(alice, "alice-subscription-roster").await;
    send_roster_get(bob, "bob-subscription-roster").await;

    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice available");
    bob.send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("bob subscribes to alice");
    let subscribe = alice
        .recv_matching(|frame| {
            frame.contains("type='subscribe'") || frame.contains("type='subscribe'")
        })
        .await
        .expect("alice receives subscribe");
    assert!(
        subscribe.contains("from='bob@localhost'") || subscribe.contains("from='bob@localhost'"),
        "expected bob subscribe request, got: {subscribe}"
    );

    alice
        .send(r#"<presence xmlns="jabber:client" type="subscribed" to="bob@localhost"/>"#)
        .await
        .expect("alice approves bob");
    let subscribed = bob
        .recv_matching(|frame| {
            frame.contains("type='subscribed'") || frame.contains("type='subscribed'")
        })
        .await
        .expect("bob receives approval");
    assert!(
        subscribed.contains("from='alice@localhost'")
            || subscribed.contains("from='alice@localhost'"),
        "expected alice approval, got: {subscribed}"
    );

    bob.send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("bob available for presence updates");
}

#[tokio::test]
async fn websocket_vcard_set_then_get_roundtrips() {
    let (_server, mut client) = setup().await;

    client
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-vcard-set"><vCard xmlns="vcard-temp"><FN>Ada Lovelace</FN><NICKNAME>ada</NICKNAME></vCard></iq>"#,
        )
        .await
        .expect("send vcard set");
    let set_response = client
        .recv_matching(|frame| frame.contains("ws-vcard-set"))
        .await
        .expect("vcard set response");
    assert!(
        set_response.contains("type='result'") || set_response.contains("type='result'"),
        "expected vCard set result, got: {set_response}"
    );

    client
        .send(
            r#"<iq xmlns="jabber:client" type="get" id="ws-vcard-get"><vCard xmlns="vcard-temp"/></iq>"#,
        )
        .await
        .expect("send vcard get");
    let get_response = client
        .recv_matching(|frame| frame.contains("ws-vcard-get"))
        .await
        .expect("vcard get response");
    assert!(
        get_response.contains("Ada Lovelace"),
        "expected stored vCard, got: {get_response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_private_xml_set_then_get_roundtrips() {
    let (_server, mut client) = setup().await;

    let prefs = Element::builder("prefs", NS_TEST_PRIVATE)
        .append(
            Element::builder("theme", NS_TEST_PRIVATE)
                .append("dark")
                .build(),
        )
        .build();
    let set_iq = private_storage_iq("ws-private-set", "set", None, [prefs]);
    client.send(&set_iq).await.expect("send private set");
    let set_response = client
        .recv_matching(|frame| frame.contains("ws-private-set"))
        .await
        .expect("private set response");
    assert!(
        set_response.contains("type='result'") || set_response.contains("type='result'"),
        "expected private XML set result, got: {set_response}"
    );

    let requested = Element::builder("prefs", NS_TEST_PRIVATE).build();
    let get_iq = private_storage_iq("ws-private-get", "get", None, [requested]);
    client.send(&get_iq).await.expect("send private get");
    let get_response = client
        .recv_matching(|frame| frame.contains("ws-private-get"))
        .await
        .expect("private get response");
    assert!(
        get_response.contains("dark"),
        "expected stored private XML, got: {get_response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_private_xml_get_without_child_returns_bad_format() {
    let (_server, mut client) = setup().await;

    let get_iq = private_storage_iq("ws-private-empty", "get", None, []);
    client.send(&get_iq).await.expect("send private empty get");
    let response = client
        .recv_matching(|frame| frame.contains("ws-private-empty"))
        .await
        .expect("private empty get response");
    assert!(
        response.contains("bad-request"),
        "expected bad-request for empty private get, got: {response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_private_xml_get_with_duplicate_children_returns_bad_format() {
    let (_server, mut client) = setup().await;

    let first = Element::builder("prefs", NS_TEST_PRIVATE).build();
    let second = Element::builder("other", NS_TEST_PRIVATE).build();
    let get_iq = private_storage_iq("ws-private-duplicate", "get", None, [first, second]);
    client
        .send(&get_iq)
        .await
        .expect("send private duplicate get");
    let response = client
        .recv_matching(|frame| frame.contains("ws-private-duplicate"))
        .await
        .expect("private duplicate get response");
    assert!(
        response.contains("bad-request"),
        "expected bad-request for duplicate private get, got: {response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_private_xml_foreign_to_returns_forbidden() {
    let (_server, mut alice, _bob) = connect_alice_bob().await;

    let requested = Element::builder("prefs", NS_TEST_PRIVATE).build();
    let get_iq = private_storage_iq(
        "ws-private-foreign",
        "get",
        Some("bob@localhost"),
        [requested],
    );
    alice.send(&get_iq).await.expect("send private foreign get");
    let response = alice
        .recv_matching(|frame| frame.contains("ws-private-foreign"))
        .await
        .expect("private foreign get response");
    assert!(
        response.contains("forbidden"),
        "expected forbidden for foreign private XML access, got: {response}"
    );

    let _ = alice.close().await;
}

#[tokio::test]
async fn websocket_private_xml_malformed_stored_xml_returns_error() {
    let tempdir = tempfile::tempdir().expect("tempdir");
    let database_url = format!(
        "sqlite://{}?mode=rwc",
        tempdir.path().join("waddle.db").display()
    );
    let (_server, mut client) = setup_with_database_url(&database_url).await;

    let pool = sqlx::SqlitePool::connect(&database_url)
        .await
        .expect("connect sqlite");
    sqlx::query(
        "INSERT OR REPLACE INTO private_xml_storage (jid, namespace, xml_content) VALUES (?, ?, ?)",
    )
    .bind("admin@localhost")
    .bind("urn:waddle:test:malformed")
    .bind("<prefs xmlns='urn:waddle:test:malformed'>")
    .execute(&pool)
    .await
    .expect("insert malformed private XML");

    let requested = Element::builder("prefs", "urn:waddle:test:malformed").build();
    let get_iq = private_storage_iq("ws-private-malformed", "get", None, [requested]);
    client
        .send(&get_iq)
        .await
        .expect("send private malformed get");
    let response = client
        .recv_matching(|frame| frame.contains("ws-private-malformed"))
        .await
        .expect("private malformed get response");
    assert!(
        response.contains("internal-server-error"),
        "expected internal-server-error for malformed stored XML, got: {response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_blocking_set_then_get_returns_blocklist() {
    let (_server, mut client) = setup().await;

    client
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-block-set"><block xmlns="urn:xmpp:blocking"><item jid="spammer@localhost"/></block></iq>"#,
        )
        .await
        .expect("send block set");
    let set_response = client
        .recv_matching(|frame| frame.contains("ws-block-set"))
        .await
        .expect("blocking set response");
    assert!(
        set_response.contains("type='result'") || set_response.contains("type='result'"),
        "expected blocking set result, got: {set_response}"
    );

    client
        .send(
            r#"<iq xmlns="jabber:client" type="get" id="ws-block-get"><blocklist xmlns="urn:xmpp:blocking"/></iq>"#,
        )
        .await
        .expect("send blocklist get");
    let get_response = client
        .recv_matching(|frame| frame.contains("ws-block-get"))
        .await
        .expect("blocklist response");
    assert!(
        get_response.contains("spammer@localhost"),
        "expected blocked JID in blocklist, got: {get_response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_disco_advertises_blocking() {
    let (_server, mut client) = setup().await;

    client
        .send(
            r#"<iq xmlns="jabber:client" type="get" id="ws-blocking-disco" to="localhost"><query xmlns="http://jabber.org/protocol/disco#info"/></iq>"#,
        )
        .await
        .expect("send disco request");
    let response = client
        .recv_matching(|frame| frame.contains("ws-blocking-disco"))
        .await
        .expect("disco response");

    assert!(
        response.contains("urn:xmpp:blocking"),
        "expected blocking feature, got: {response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_blocking_updates_presence_visibility_for_subscribed_contact() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    establish_subscription_to_alice(&mut alice, &mut bob).await;

    alice
        .send(
            r#"<presence xmlns="jabber:client"><show>chat</show><status>visible before block</status><priority>7</priority></presence>"#,
        )
        .await
        .expect("alice sends current presence");
    let initial_presence = bob
        .recv_matching(|frame| frame.contains("visible before block"))
        .await
        .expect("bob receives initial presence");
    assert!(
        initial_presence.contains("from='alice@localhost/")
            || initial_presence.contains("from='alice@localhost/"),
        "expected alice presence before blocking, got: {initial_presence}"
    );

    alice
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-block-presence"><block xmlns="urn:xmpp:blocking"><item jid="bob@localhost"/></block></iq>"#,
        )
        .await
        .expect("alice blocks bob");
    let block_response = alice
        .recv_matching(|frame| frame.contains("ws-block-presence"))
        .await
        .expect("blocking response");
    assert!(
        block_response.contains("type='result'") || block_response.contains("type='result'"),
        "expected blocking result, got: {block_response}"
    );
    let unavailable = bob
        .recv_matching(|frame| {
            (frame.contains("type='unavailable'") || frame.contains("type='unavailable'"))
                && (frame.contains("from='alice@localhost/")
                    || frame.contains("from='alice@localhost/"))
        })
        .await
        .expect("bob receives unavailable presence after block");
    assert!(
        unavailable.contains("to='bob@localhost'") || unavailable.contains("to='bob@localhost'"),
        "expected unavailable presence addressed to bob, got: {unavailable}"
    );

    alice
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-unblock-presence"><unblock xmlns="urn:xmpp:blocking"><item jid="bob@localhost"/></unblock></iq>"#,
        )
        .await
        .expect("alice unblocks bob");
    let unblock_response = alice
        .recv_matching(|frame| frame.contains("ws-unblock-presence"))
        .await
        .expect("unblocking response");
    assert!(
        unblock_response.contains("type='result'") || unblock_response.contains("type='result'"),
        "expected unblocking result, got: {unblock_response}"
    );
    let restored_presence = bob
        .recv_matching(|frame| frame.contains("visible before block"))
        .await
        .expect("bob receives current presence after unblock");
    assert!(
        restored_presence.contains("from='alice@localhost/")
            || restored_presence.contains("from='alice@localhost/"),
        "expected alice current presence after unblock, got: {restored_presence}"
    );

    assert_no_frame_matching(
        &mut bob,
        Duration::from_millis(250),
        |frame| frame.contains("type='unavailable'") || frame.contains("type='unavailable'"),
        "bob should not receive extra unavailable presence after unblock",
    )
    .await;

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn websocket_push_enable_rejects_external_service_until_publish_is_wired() {
    let (_server, mut client) = setup().await;
    let enable = push_enable_iq(
        "ws-push-enable",
        "push-provider.localhost",
        "web",
        Some(publish_options_form([xdata_field(
            "secret",
            "opaque-service-secret",
        )])),
    );

    client.send(&enable).await.expect("send push enable");
    let enable_response = client
        .recv_matching(|frame| frame.contains("ws-push-enable"))
        .await
        .expect("push enable response");
    assert!(
        enable_response.contains("type='error'") || enable_response.contains("type='error'"),
        "expected push enable error, got: {enable_response}"
    );
    assert!(
        enable_response.contains("service-unavailable"),
        "expected service-unavailable for unsupported external push service, got: {enable_response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_push_enable_rejects_external_service_without_provider_credentials() {
    let (_server, mut client) = setup().await;
    let enable = push_enable_iq(
        "ws-push-no-provider-data",
        "push-provider.localhost",
        "web",
        None,
    );

    client.send(&enable).await.expect("send push enable");
    let response = client
        .recv_matching(|frame| frame.contains("ws-push-no-provider-data"))
        .await
        .expect("push enable response");

    assert!(
        response.contains("type='error'") || response.contains("type='error'"),
        "expected push enable error for unsupported external push service, got: {response}"
    );
    assert!(
        response.contains("service-unavailable"),
        "expected service-unavailable for unsupported external push service, got: {response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_push_enable_rejects_provider_credentials_in_publish_options() {
    let (_server, mut client) = setup().await;
    let enable = push_enable_iq(
        "ws-push-provider-data",
        "push-provider.localhost",
        "web",
        Some(publish_options_form([
            xdata_field("endpoint", "https://updates.push.services.mozilla.com/abc"),
            xdata_field("p256dh", "p256dh-key"),
            xdata_field("auth", "auth-secret"),
        ])),
    );

    client
        .send(&enable)
        .await
        .expect("send push enable with provider data");
    let response = client
        .recv_matching(|frame| frame.contains("ws-push-provider-data"))
        .await
        .expect("push enable response");

    assert!(
        response.contains("type='error'") || response.contains("type='error'"),
        "expected push enable error for provider credentials, got: {response}"
    );
    assert!(
        response.contains("service-unavailable"),
        "expected service-unavailable for unsupported external push service, got: {response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_push_enable_rejects_invalid_service_jid() {
    let (_server, mut client) = setup().await;
    let enable = push_enable_iq("ws-push-invalid-jid", "not a jid", "web", None);

    client
        .send(&enable)
        .await
        .expect("send push enable with invalid jid");
    let response = client
        .recv_matching(|frame| frame.contains("ws-push-invalid-jid"))
        .await
        .expect("push enable response");

    assert!(
        response.contains("type='error'") || response.contains("type='error'"),
        "expected push enable error for invalid service jid, got: {response}"
    );
    assert!(
        response.contains("bad-request"),
        "expected bad-request for invalid service jid, got: {response}"
    );

    let _ = client.close().await;
}

/// The retired `urn:xmpp:isr:0` `<token-request/>` IQ falls through to the
/// ordinary unhandled-IQ catch-all (`feature-not-implemented`), exactly like
/// any other unsupported payload.
#[tokio::test]
async fn websocket_legacy_isr_token_request_iq_is_gone() {
    let (_server, mut client) = setup().await;

    client
        .send(
            r#"<iq xmlns="jabber:client" type="get" id="ws-isr-token"><token-request xmlns="urn:xmpp:isr:0"/></iq>"#,
        )
        .await
        .expect("send legacy ISR token-request IQ");
    let response = client
        .recv_matching(|frame| frame.contains("ws-isr-token"))
        .await
        .expect("IQ error response");
    assert!(
        response.contains("type='error'"),
        "expected an error reply for the retired IQ path, got: {response}"
    );
    assert!(
        response.contains("feature-not-implemented"),
        "expected feature-not-implemented (the unhandled-IQ catch-all), got: {response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_user_and_channel_search_return_results() {
    let (_server, mut client) = setup().await;

    client
        .send(
            r#"<iq xmlns="jabber:client" type="get" id="ws-user-search-form" to="localhost"><query xmlns="jabber:iq:search"/></iq>"#,
        )
        .await
        .expect("send user search form request");
    let form = client
        .recv_matching(|frame| frame.contains("ws-user-search-form"))
        .await
        .expect("user search form response");
    assert!(
        form.contains("jabber:iq:search") && form.contains("instructions"),
        "expected user search form, got: {form}"
    );
    assert!(
        form.contains("<nick") && !form.contains("<email"),
        "expected username-only user search form, got: {form}"
    );

    client
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-user-search" to="localhost"><query xmlns="jabber:iq:search"><nick>admin</nick></query></iq>"#,
        )
        .await
        .expect("send user search request");
    let users = client
        .recv_matching(|frame| frame.contains("ws-user-search"))
        .await
        .expect("user search response");
    assert!(
        users.contains("admin@localhost"),
        "expected fixed test account in user search results, got: {users}"
    );
    assert!(
        !users.contains("<email") && !users.contains("admin@localhost</email>"),
        "expected user search results to omit email addresses, got: {users}"
    );

    client
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-user-search-empty" to="localhost"><query xmlns="jabber:iq:search"><nick> </nick></query></iq>"#,
        )
        .await
        .expect("send empty user search request");
    let empty_search = client
        .recv_matching(|frame| frame.contains("ws-user-search-empty"))
        .await
        .expect("empty user search response");
    assert!(
        empty_search.contains("type='error'") || empty_search.contains("type='error'"),
        "expected empty user search to fail, got: {empty_search}"
    );
    assert!(
        empty_search.contains("bad-request"),
        "expected empty user search bad-request, got: {empty_search}"
    );

    client
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-user-search-wildcard" to="localhost"><query xmlns="jabber:iq:search"><nick>%%</nick></query></iq>"#,
        )
        .await
        .expect("send wildcard user search request");
    let wildcard_search = client
        .recv_matching(|frame| frame.contains("ws-user-search-wildcard"))
        .await
        .expect("wildcard user search response");
    assert!(
        wildcard_search.contains("type='result'") || wildcard_search.contains("type='result'"),
        "expected wildcard user search to return an empty result, got: {wildcard_search}"
    );
    assert!(
        !wildcard_search.contains("admin@localhost"),
        "expected SQL wildcards to be escaped, got: {wildcard_search}"
    );

    client
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-user-search-wrong-ns" to="localhost"><query xmlns="jabber:iq:search"><nick xmlns="urn:evil">admin</nick></query></iq>"#,
        )
        .await
        .expect("send wrong-namespace user search request");
    let wrong_ns_search = client
        .recv_matching(|frame| frame.contains("ws-user-search-wrong-ns"))
        .await
        .expect("wrong-namespace user search response");
    assert!(
        wrong_ns_search.contains("type='error'") || wrong_ns_search.contains("type='error'"),
        "expected wrong-namespace user search to fail, got: {wrong_ns_search}"
    );
    assert!(
        wrong_ns_search.contains("bad-request"),
        "expected wrong-namespace user search bad-request, got: {wrong_ns_search}"
    );

    client
        .send(
            r#"<iq xmlns="jabber:client" type="get" id="ws-channel-search" to="muc.localhost"><search xmlns="urn:xmpp:channel-search:0:search"><set xmlns="http://jabber.org/protocol/rsm"><max>5</max></set><x xmlns="jabber:x:data" type="submit"><field var="FORM_TYPE" type="hidden"><value>urn:xmpp:channel-search:0:search-params</value></field><field var="q"><value></value></field></x></search></iq>"#,
        )
        .await
        .expect("send channel search request");
    let channels = client
        .recv_matching(|frame| frame.contains("ws-channel-search"))
        .await
        .expect("channel search response");
    assert!(
        (channels.contains("type='result'") || channels.contains("type='result'"))
            && channels.contains("urn:xmpp:channel-search:0:search"),
        "expected channel search result, got: {channels}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_full_jid_iq_routes_to_bound_resource() {
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();

    let mut sender = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("iq-sender-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("sender connection");
    let mut recipient = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("iq-recipient-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("recipient connection");
    let recipient_jid = recipient
        .full_jid
        .clone()
        .expect("recipient should have a full JID");

    sender
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="ws-full-jid-route" to="{recipient_jid}"><ping xmlns="urn:xmpp:ping"/></iq>"#
        ))
        .await
        .expect("send routed IQ");
    let routed = recipient
        .recv_matching(|frame| frame.contains("ws-full-jid-route"))
        .await
        .expect("recipient routed IQ");
    assert!(
        routed.contains("from='admin@localhost/") || routed.contains("from='admin@localhost/"),
        "expected routed IQ from sender resource, got: {routed}"
    );
    assert!(
        routed.contains("urn:xmpp:ping"),
        "expected ping payload in routed IQ, got: {routed}"
    );

    let _ = recipient.close().await;
    let _ = sender.close().await;
}

#[tokio::test]
async fn websocket_blocking_prevents_full_jid_iq_routing() {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);

    let mut sender = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("iq-block-sender-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("sender connection");
    let mut recipient = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("iq-block-recipient-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("recipient connection");
    let recipient_jid = recipient
        .full_jid
        .clone()
        .expect("recipient should have a full JID");

    recipient
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-block-bob-iq"><block xmlns="urn:xmpp:blocking"><item jid="bob@localhost"/></block></iq>"#,
        )
        .await
        .expect("send block set");
    let block_response = recipient
        .recv_matching(|frame| frame.contains("ws-block-bob-iq"))
        .await
        .expect("blocking set response");
    assert!(
        block_response.contains("type='result'") || block_response.contains("type='result'"),
        "expected blocking set result, got: {block_response}"
    );

    sender
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="ws-blocked-full-jid" to="{recipient_jid}"><ping xmlns="urn:xmpp:ping"/></iq>"#
        ))
        .await
        .expect("send blocked routed IQ");
    let sender_error = sender
        .recv_matching(|frame| frame.contains("ws-blocked-full-jid"))
        .await
        .expect("blocked IQ error response");
    assert!(
        sender_error.contains("service-unavailable"),
        "expected service-unavailable for blocked full-JID IQ, got: {sender_error}"
    );
    assert_no_frame_matching(
        &mut recipient,
        Duration::from_millis(250),
        |frame| frame.contains("ws-blocked-full-jid"),
        "recipient should not receive blocked full-JID IQ",
    )
    .await;

    let _ = recipient.close().await;
    let _ = sender.close().await;
}

#[tokio::test]
async fn websocket_direct_muc_invite_routes_normal_message() {
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();

    let mut sender = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("invite-sender-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("sender connection");
    let mut recipient = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("invite-recipient-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("recipient connection");
    let recipient_jid = recipient
        .full_jid
        .clone()
        .expect("recipient should have a full JID");

    sender
        .send(&format!(
            r#"<message xmlns="jabber:client" id="ws-direct-invite" type="normal" to="{recipient_jid}"><x xmlns="jabber:x:conference" jid="room@muc.localhost" reason="join us"/></message>"#
        ))
        .await
        .expect("send direct invite");
    let invite = recipient
        .recv_matching(|frame| frame.contains("ws-direct-invite"))
        .await
        .expect("recipient invite");
    assert!(
        invite.contains("jabber:x:conference") && !invite.contains("type='chat'"),
        "expected non-chat direct MUC invite, got: {invite}"
    );

    let _ = recipient.close().await;
    let _ = sender.close().await;
}

#[tokio::test]
async fn websocket_blocking_prevents_direct_message_delivery() {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);

    let mut sender = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("msg-block-sender-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("sender connection");
    let mut recipient = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("msg-block-recipient-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("recipient connection");
    let recipient_jid = recipient
        .full_jid
        .clone()
        .expect("recipient should have a full JID");

    recipient
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-block-message-sender"><block xmlns="urn:xmpp:blocking"><item jid="bob@localhost"/></block></iq>"#,
        )
        .await
        .expect("send block set");
    let block_response = recipient
        .recv_matching(|frame| frame.contains("ws-block-message-sender"))
        .await
        .expect("blocking set response");
    assert!(
        block_response.contains("type='result'") || block_response.contains("type='result'"),
        "expected blocking set result, got: {block_response}"
    );

    sender
        .send(&format!(
            r#"<message xmlns="jabber:client" id="ws-blocked-message" type="chat" to="{recipient_jid}"><body>blocked</body></message>"#
        ))
        .await
        .expect("send blocked direct message");
    assert_no_frame_matching(
        &mut recipient,
        Duration::from_millis(250),
        |frame| frame.contains("ws-blocked-message"),
        "recipient should not receive blocked direct message",
    )
    .await;

    let _ = recipient.close().await;
    let _ = sender.close().await;
}

#[tokio::test]
async fn websocket_normal_message_routes_as_direct_message() {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);

    let mut sender = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("normal-sender-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("sender connection");
    let mut recipient = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("normal-recipient-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("recipient connection");

    sender
        .send(
            r#"<message xmlns="jabber:client" id="ws-normal-message" type="normal" to="alice@localhost"><body>hello normal</body></message>"#,
        )
        .await
        .expect("send normal direct message");
    let delivered = recipient
        .recv_matching(|frame| {
            frame.contains("ws-normal-message")
                && (frame.contains("<body>hello normal</body>")
                    || frame.contains("type='normal'")
                    || frame.contains("type='normal'"))
        })
        .await
        .expect("recipient normal message");
    assert!(
        delivered.contains("hello normal") && delivered.contains("from='bob@localhost/"),
        "expected routed normal direct message, got: {delivered}"
    );

    let _ = recipient.close().await;
    let _ = sender.close().await;
}

#[tokio::test]
async fn websocket_pubsub_subscribe_and_unsubscribe_acknowledge_spaces_node() {
    let (_server, mut client) = setup().await;

    // Create the spaces node first (requires server owner; admin is owner in TestServer).
    client
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-create-space" to="spaces.localhost"><pubsub xmlns="http://jabber.org/protocol/pubsub"><create node="space"/></pubsub></iq>"#,
        )
        .await
        .expect("send pubsub create node");
    let created = client
        .recv_matching(|frame| frame.contains("ws-create-space"))
        .await
        .expect("pubsub create node response");
    assert!(
        created.contains("type='result'") || created.contains("type='result'"),
        "expected pubsub create-node result, got: {created}"
    );

    client
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-pubsub-sub" to="spaces.localhost"><pubsub xmlns="http://jabber.org/protocol/pubsub"><subscribe node="space" jid="admin@localhost"/></pubsub></iq>"#,
        )
        .await
        .expect("send pubsub subscribe");
    let sub = client
        .recv_matching(|frame| frame.contains("ws-pubsub-sub"))
        .await
        .expect("pubsub subscribe response");
    assert!(
        sub.contains("type='result'") || sub.contains("type='result'"),
        "expected pubsub subscribe result, got: {sub}"
    );

    client
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-pubsub-unsub" to="spaces.localhost"><pubsub xmlns="http://jabber.org/protocol/pubsub"><unsubscribe node="space" jid="admin@localhost"/></pubsub></iq>"#,
        )
        .await
        .expect("send pubsub unsubscribe");
    let unsub = client
        .recv_matching(|frame| frame.contains("ws-pubsub-unsub"))
        .await
        .expect("pubsub unsubscribe response");
    assert!(
        unsub.contains("type='result'") || unsub.contains("type='result'"),
        "expected pubsub unsubscribe result, got: {unsub}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_muc_self_ping_succeeds_for_joined_occupant() {
    let (_server, mut client) = setup().await;
    let room = format!("self-ping-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    client
        .send(&format!(
            r#"<presence xmlns="jabber:client" to="{room}/admin"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");

    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="ws-muc-self-ping" to="{room}/admin"><ping xmlns="urn:xmpp:ping"/></iq>"#
        ))
        .await
        .expect("send MUC self-ping");
    let response = client
        .recv_matching(|frame| frame.contains("ws-muc-self-ping"))
        .await
        .expect("self-ping response");
    assert!(
        response.contains("type='result'") || response.contains("type='result'"),
        "expected MUC self-ping result, got: {response}"
    );

    let _ = client.close().await;
}

/// XEP-0410 server optimization (#1254): a self-ping to a room with no
/// live actor (never created, reaped, or sealed-dormant) MUST be the
/// authoritative not-joined answer `<not-acceptable/>` so the client
/// rejoins. The previous `<item-not-found/>` is defined by XEP-0410
/// clients as "still joined (mid-nick-change)" — after a reap the
/// client never rejoined and silently stopped receiving messages.
#[tokio::test]
async fn websocket_muc_self_ping_to_reaped_room_returns_not_acceptable() {
    let (_server, mut client) = setup().await;
    let room = format!("self-ping-reaped-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="ws-muc-self-ping-reaped" to="{room}/admin"><ping xmlns="urn:xmpp:ping"/></iq>"#
        ))
        .await
        .expect("send MUC self-ping");
    let response = client
        .recv_matching(|frame| frame.contains("ws-muc-self-ping-reaped"))
        .await
        .expect("self-ping response");
    assert!(
        response.contains("not-acceptable"),
        "expected XEP-0410 not-joined answer <not-acceptable/>, got: {response}"
    );
    assert!(
        !response.contains("item-not-found"),
        "item-not-found reads as still-joined per XEP-0410; got: {response}"
    );

    let _ = client.close().await;
}

/// XEP-0410 (#1253): a self-ping from a session that is NOT joined to
/// an EXISTING room also gets `<not-acceptable/>`.
#[tokio::test]
async fn websocket_muc_self_ping_from_non_occupant_returns_not_acceptable() {
    let (_server, mut client) = setup().await;
    let room = format!("self-ping-nonocc-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());

    client
        .send(&format!(
            r#"<presence xmlns="jabber:client" to="{room}/admin"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");

    // Ping a nick nobody holds — the pinging session is not joined
    // under it, so the optimized answer is not-acceptable.
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="get" id="ws-muc-self-ping-nonocc" to="{room}/other-nick"><ping xmlns="urn:xmpp:ping"/></iq>"#
        ))
        .await
        .expect("send MUC self-ping");
    let response = client
        .recv_matching(|frame| frame.contains("ws-muc-self-ping-nonocc"))
        .await
        .expect("self-ping response");
    assert!(
        response.contains("not-acceptable"),
        "expected XEP-0410 not-joined answer <not-acceptable/>, got: {response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_muc_room_disco_advertises_self_ping_optimization_only_on_rooms() {
    let (_server, mut client) = setup().await;
    let room = format!("self-ping-disco-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    let muc_service = format!("muc.{DOMAIN}");
    let feature = "http://jabber.org/protocol/muc#self-ping-optimization";

    client
        .send(&format!(
            r#"<presence xmlns="jabber:client" to="{room}/admin"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");

    let service_response = disco_info_query(&mut client, &muc_service, "ws-muc-self-ping-svc")
        .await
        .expect("service disco#info response");
    assert!(
        !service_response.contains(feature),
        "muc service disco must not advertise XEP-0410 room feature: {service_response}"
    );

    let room_response = disco_info_query(&mut client, &room, "ws-muc-self-ping-room")
        .await
        .expect("room disco#info response");
    assert!(
        room_response.contains(feature),
        "muc room disco missing XEP-0410 feature: {room_response}"
    );

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_rejects_spoofed_muc_domain_presence() {
    let (_server, mut client) = setup().await;
    let room = format!("spoof-{}@muc.localhost.evil", uuid::Uuid::new_v4());

    client
        .send(&format!(
            r#"<presence xmlns="jabber:client" to="{room}/admin"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send spoofed MUC presence");
    assert_no_frame_matching(
        &mut client,
        Duration::from_millis(250),
        |frame| frame.contains("muc.localhost.evil"),
        "spoofed MUC domain should not be treated as a local room",
    )
    .await;

    let _ = client.close().await;
}
