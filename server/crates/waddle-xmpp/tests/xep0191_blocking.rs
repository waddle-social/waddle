#![recursion_limit = "256"]

//! XEP-0191: Blocking Command dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, ping_query, RawXmppClient,
    TestServer, DEFAULT_TIMEOUT,
};
use minidom::Element;

fn xml_string(element: Element) -> String {
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize xml");
    String::from_utf8(buf).expect("utf8 xml")
}

fn block_iq(id: &str, jid: &str) -> String {
    xml_string(
        Element::builder("iq", "jabber:client")
            .attr("type", "set")
            .attr("id", id)
            .append(
                Element::builder("block", "urn:xmpp:blocking").append(
                    Element::builder("item", "urn:xmpp:blocking")
                        .attr("jid", jid)
                        .build(),
                ),
            )
            .build(),
    )
}

fn blocking_get(id: &str) -> String {
    xml_string(
        Element::builder("iq", "jabber:client")
            .attr("type", "get")
            .attr("id", id)
            .append(Element::builder("blocklist", "urn:xmpp:blocking").build())
            .build(),
    )
}

fn unblock_iq(id: &str, jid: &str) -> String {
    xml_string(
        Element::builder("iq", "jabber:client")
            .attr("type", "set")
            .attr("id", id)
            .append(
                Element::builder("unblock", "urn:xmpp:blocking").append(
                    Element::builder("item", "urn:xmpp:blocking")
                        .attr("jid", jid)
                        .build(),
                ),
            )
            .build(),
    )
}

fn subscribe_presence(to: &str, status: &str) -> String {
    xml_string(
        Element::builder("presence", "jabber:client")
            .attr("type", "subscribe")
            .attr("to", to)
            .append(
                Element::builder("status", "jabber:client")
                    .append(status)
                    .build(),
            )
            .build(),
    )
}

fn ping_iq(id: &str, to: &str) -> String {
    xml_string(
        Element::builder("iq", "jabber:client")
            .attr("type", "get")
            .attr("id", id)
            .attr("to", to)
            .append(Element::builder("ping", "urn:xmpp:ping").build())
            .build(),
    )
}

async fn block_jid(client: &mut RawXmppClient, jid: &str, id: &str) {
    client.send(&block_iq(id, jid)).await.expect("send block");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("block response");
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Block should succeed, got: {}",
        response
    );
    client.clear();
}

#[tokio::test]
async fn xep0191_server_disco_advertises_blocking() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "localhost", "block-disco-1")
        .await
        .expect("disco response");

    assert!(
        response.contains("urn:xmpp:blocking"),
        "Expected blocking feature, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0191_get_blocklist_returns_empty() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(&blocking_get("blocklist-1"))
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ, got: {}",
        response
    );
    assert!(
        response.contains("urn:xmpp:blocking"),
        "Expected blocking namespace, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0191_block_jid_returns_success() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(&block_iq("block-1", "spammer@example.com"))
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ for block, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0191_unblock_jid_returns_success() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(&unblock_iq("unblock-1", "friend@example.com"))
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ for unblock, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0191_blocked_subscription_presence_is_dropped() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");

    block_jid(&mut alice, "bob@localhost", "block-bob-subscribe").await;

    bob.send(&subscribe_presence("alice@localhost", "please add me"))
        .await
        .expect("send subscribe");

    tokio::time::sleep(std::time::Duration::from_millis(300)).await;

    let response = ping_query(&mut alice, "localhost", "after-blocked-subscribe")
        .await
        .expect("ping");
    assert!(
        !response.contains("please add me"),
        "Blocked subscribe presence should not be delivered, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0191_blocked_full_jid_iq_returns_error() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    let alice_jid = establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");

    block_jid(&mut alice, "bob@localhost", "block-bob-full-iq").await;

    bob.send(&ping_iq("blocked-full-iq-1", &alice_jid))
        .await
        .expect("send iq");
    let response = bob
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='error'") || response.contains("type=\"error\""),
        "Expected error IQ, got: {}",
        response
    );
    assert!(
        response.contains("service-unavailable"),
        "Expected service-unavailable error, got: {}",
        response
    );
}
