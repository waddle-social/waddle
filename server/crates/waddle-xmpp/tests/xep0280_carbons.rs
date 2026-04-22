#![recursion_limit = "256"]

//! XEP-0280 Message Carbons integration suite.
//!
//! These tests exercise the per-resource opt-in requirement of XEP-0280 §5:
//! "Carbons MUST be enabled for each online resource separately."
//!
//! The server must only deliver `<sent>` and `<received>` carbon wrappers to
//! resources that have explicitly sent `<enable xmlns='urn:xmpp:carbons:2'/>`.
//! Resources that never opted in (or disabled carbons after enabling) must not
//! receive carbon-wrapped copies of messages from their siblings.

mod common;

use std::time::Duration;

use common::{establish_bound_session, init_test_env, RawXmppClient, TestServer, DEFAULT_TIMEOUT};

/// How long to wait for a carbon that should NOT arrive. Long enough that
/// the server has had any reasonable chance to misdeliver, short enough that
/// the test suite stays fast.
const CARBON_SETTLE: Duration = Duration::from_millis(400);

/// Establish a bound session, then send initial `<presence/>` so the server
/// marks the resource available and will route bare-JID addressed messages.
async fn bind_and_announce(
    client: &mut RawXmppClient,
    server: &TestServer,
    user: &str,
    resource: &str,
) -> String {
    let full_jid = establish_bound_session(client, server, user, resource)
        .await
        .unwrap_or_else(|e| panic!("bind {}/{}: {}", user, resource, e));
    client
        .send("<presence xmlns='jabber:client'/>")
        .await
        .expect("send initial presence");
    full_jid
}

async fn enable_carbons(client: &mut RawXmppClient, id: &str) {
    client
        .send(&format!(
            "<iq type='set' id='{}' xmlns='jabber:client'>\
                <enable xmlns='urn:xmpp:carbons:2'/>\
            </iq>",
            id
        ))
        .await
        .expect("send carbons enable");
    // IDs are unique; match the raw id string so we're agnostic to single vs
    // double quote styling in the serialized stanza.
    client
        .read_until(id, DEFAULT_TIMEOUT)
        .await
        .expect("carbons enable result");
    client.clear();
}

async fn disable_carbons(client: &mut RawXmppClient, id: &str) {
    client
        .send(&format!(
            "<iq type='set' id='{}' xmlns='jabber:client'>\
                <disable xmlns='urn:xmpp:carbons:2'/>\
            </iq>",
            id
        ))
        .await
        .expect("send carbons disable");
    client
        .read_until(id, DEFAULT_TIMEOUT)
        .await
        .expect("carbons disable result");
    client.clear();
}

async fn set_presence_priority(client: &mut RawXmppClient, priority: i8) {
    client
        .send(&format!(
            "<presence xmlns='jabber:client'><priority>{priority}</priority></presence>"
        ))
        .await
        .expect("send presence with priority");
}

/// Drain any stanzas delivered within `window` so later assertions examine a
/// fresh buffer.
async fn settle(client: &mut RawXmppClient, window: Duration) {
    let _ = client.read(window).await;
}

#[tokio::test]
async fn sent_carbon_skipped_for_resource_that_did_not_opt_in() {
    init_test_env();
    let server = TestServer::start().await;

    // Alice has two resources. Desktop opts in; mobile does not.
    let mut alice_desktop = RawXmppClient::connect(server.addr).await.expect("connect");
    bind_and_announce(&mut alice_desktop, &server, "alice", "desktop").await;
    enable_carbons(&mut alice_desktop, "carbons-enable-desktop").await;

    let mut alice_mobile = RawXmppClient::connect(server.addr).await.expect("connect");
    bind_and_announce(&mut alice_mobile, &server, "alice", "mobile").await;

    // Bob exists as the recipient so the sent message has somewhere to go.
    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    bind_and_announce(&mut bob, &server, "bob", "laptop").await;

    // Desktop sends a chat message to Bob.
    alice_desktop
        .send(
            "<message type='chat' to='bob@localhost' id='dm-1' xmlns='jabber:client'>\
                <body>hello bob</body>\
            </message>",
        )
        .await
        .expect("send dm");

    // Bob must receive the original message (sanity check on routing).
    bob.read_until("hello bob", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives original");

    // Mobile did NOT opt in; give the server plenty of time to (mis)deliver.
    settle(&mut alice_mobile, CARBON_SETTLE).await;
    let mobile_buffer = alice_mobile.take_buffer();
    assert!(
        !mobile_buffer.contains("urn:xmpp:carbons:2"),
        "mobile resource did not opt into carbons but received carbon-wrapped stanza:\n{}",
        mobile_buffer
    );
}

#[tokio::test]
async fn sent_carbon_delivered_to_opted_in_sibling() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice_desktop = RawXmppClient::connect(server.addr).await.expect("connect");
    bind_and_announce(&mut alice_desktop, &server, "alice", "desktop").await;
    enable_carbons(&mut alice_desktop, "carbons-enable-desktop").await;

    let mut alice_mobile = RawXmppClient::connect(server.addr).await.expect("connect");
    bind_and_announce(&mut alice_mobile, &server, "alice", "mobile").await;
    enable_carbons(&mut alice_mobile, "carbons-enable-mobile").await;

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    bind_and_announce(&mut bob, &server, "bob", "laptop").await;

    alice_desktop
        .send(
            "<message type='chat' to='bob@localhost' id='dm-2' xmlns='jabber:client'>\
                <body>synced note</body>\
            </message>",
        )
        .await
        .expect("send dm");

    bob.read_until("synced note", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives original");

    let mobile_buffer = alice_mobile
        .read_until("synced note", DEFAULT_TIMEOUT)
        .await
        .expect("mobile receives sent carbon");
    assert!(
        mobile_buffer.contains("<sent"),
        "mobile should receive a <sent> carbon wrapper, got:\n{}",
        mobile_buffer
    );
    assert!(
        mobile_buffer.contains("urn:xmpp:carbons:2"),
        "carbon stanza should carry urn:xmpp:carbons:2, got:\n{}",
        mobile_buffer
    );
}

#[tokio::test]
async fn sent_carbon_stops_after_disable() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice_desktop = RawXmppClient::connect(server.addr).await.expect("connect");
    bind_and_announce(&mut alice_desktop, &server, "alice", "desktop").await;
    enable_carbons(&mut alice_desktop, "carbons-enable-desktop").await;

    let mut alice_mobile = RawXmppClient::connect(server.addr).await.expect("connect");
    bind_and_announce(&mut alice_mobile, &server, "alice", "mobile").await;
    enable_carbons(&mut alice_mobile, "carbons-enable-mobile").await;
    disable_carbons(&mut alice_mobile, "carbons-disable-mobile").await;

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    bind_and_announce(&mut bob, &server, "bob", "laptop").await;

    alice_desktop
        .send(
            "<message type='chat' to='bob@localhost' id='dm-3' xmlns='jabber:client'>\
                <body>after disable</body>\
            </message>",
        )
        .await
        .expect("send dm");

    bob.read_until("after disable", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives original");

    settle(&mut alice_mobile, CARBON_SETTLE).await;
    let mobile_buffer = alice_mobile.take_buffer();
    assert!(
        !mobile_buffer.contains("urn:xmpp:carbons:2"),
        "mobile disabled carbons but still received carbon stanza:\n{}",
        mobile_buffer
    );
}

#[tokio::test]
async fn received_carbon_skipped_for_resource_that_did_not_opt_in() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice_desktop = RawXmppClient::connect(server.addr).await.expect("connect");
    let desktop_jid = bind_and_announce(&mut alice_desktop, &server, "alice", "desktop").await;
    enable_carbons(&mut alice_desktop, "carbons-enable-desktop").await;

    let mut alice_mobile = RawXmppClient::connect(server.addr).await.expect("connect");
    bind_and_announce(&mut alice_mobile, &server, "alice", "mobile").await;

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    bind_and_announce(&mut bob, &server, "bob", "laptop").await;

    // Bob targets Alice's desktop resource specifically so that received carbons
    // are generated for the other resources (see send_received_carbons_to_user).
    bob.send(&format!(
        "<message type='chat' to='{}' id='dm-in-1' xmlns='jabber:client'>\
            <body>direct to desktop</body>\
        </message>",
        desktop_jid
    ))
    .await
    .expect("bob sends");

    alice_desktop
        .read_until("direct to desktop", DEFAULT_TIMEOUT)
        .await
        .expect("desktop receives original");

    settle(&mut alice_mobile, CARBON_SETTLE).await;
    let mobile_buffer = alice_mobile.take_buffer();
    assert!(
        !mobile_buffer.contains("urn:xmpp:carbons:2"),
        "mobile did not opt into carbons but received a <received> carbon:\n{}",
        mobile_buffer
    );
}

#[tokio::test]
async fn received_carbon_delivered_for_bare_jid_to_opted_in_non_target_resource() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice_desktop = RawXmppClient::connect(server.addr).await.expect("connect");
    bind_and_announce(&mut alice_desktop, &server, "alice", "desktop").await;
    enable_carbons(&mut alice_desktop, "carbons-enable-desktop").await;
    set_presence_priority(&mut alice_desktop, 1).await;

    let mut alice_mobile = RawXmppClient::connect(server.addr).await.expect("connect");
    bind_and_announce(&mut alice_mobile, &server, "alice", "mobile").await;
    set_presence_priority(&mut alice_mobile, 5).await;

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    bind_and_announce(&mut bob, &server, "bob", "laptop").await;

    bob.send(
        "<message type='chat' to='alice@localhost' id='dm-in-2' xmlns='jabber:client'>\
            <body>priority bare routing</body>\
        </message>",
    )
    .await
    .expect("bob sends");

    alice_mobile
        .read_until("priority bare routing", DEFAULT_TIMEOUT)
        .await
        .expect("highest-priority resource receives original");

    let desktop_buffer = alice_desktop
        .read_until("priority bare routing", DEFAULT_TIMEOUT)
        .await
        .expect("desktop receives received carbon for bare-JID delivery");
    assert!(
        desktop_buffer.contains("<received"),
        "desktop should receive a <received> carbon wrapper, got:\n{}",
        desktop_buffer
    );
    assert!(
        desktop_buffer.contains("urn:xmpp:carbons:2"),
        "received carbon stanza should carry urn:xmpp:carbons:2, got:\n{}",
        desktop_buffer
    );
}
