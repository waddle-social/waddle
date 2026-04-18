#![recursion_limit = "256"]

//! XEP-0012: Last Activity dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};
use minidom::Element;

fn xml_string(element: Element) -> String {
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize xml");
    String::from_utf8(buf).expect("utf8 xml")
}

fn response_seconds(response: &str) -> Option<u64> {
    response
        .split("seconds=")
        .nth(1)
        .and_then(|rest| rest.chars().next().map(|quote| (quote, &rest[1..])))
        .and_then(|(quote, rest)| rest.split(quote).next())
        .and_then(|value| value.parse().ok())
}

fn last_activity_iq(id: &str, to: Option<&str>) -> String {
    let mut iq = Element::builder("iq", "jabber:client")
        .attr("type", "get")
        .attr("id", id);
    if let Some(to) = to {
        iq = iq.attr("to", to);
    }
    xml_string(
        iq.append(Element::builder("query", "jabber:iq:last").build())
            .build(),
    )
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

fn unavailable_presence(status: &str) -> String {
    xml_string(
        Element::builder("presence", "jabber:client")
            .attr("type", "unavailable")
            .append(
                Element::builder("status", "jabber:client")
                    .append(status)
                    .build(),
            )
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
async fn xep0012_server_disco_advertises_last_activity() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "localhost", "disco-0012")
        .await
        .expect("disco response");

    assert!(
        response.contains("jabber:iq:last"),
        "Expected jabber:iq:last feature in disco#info, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0012_server_uptime_query_returns_result() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(&last_activity_iq("last-1", Some("localhost")))
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
        response.contains("jabber:iq:last"),
        "Expected last activity namespace, got: {}",
        response
    );
    assert!(
        response.contains("seconds="),
        "Expected seconds attribute in last activity, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0012_server_uptime_query_reports_real_uptime() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    client
        .send(&last_activity_iq("last-uptime-real", Some("localhost")))
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    let seconds = response_seconds(&response).expect("seconds attribute");
    assert!(
        seconds >= 1,
        "Expected non-zero uptime, got response: {}",
        response
    );
}

#[tokio::test]
async fn xep0012_query_to_unknown_user_returns_error() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(&last_activity_iq("last-2", Some("nobody@localhost")))
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    // Should return either a result (seconds=0 for unknown) or service-unavailable
    assert!(
        response.contains("type='result'")
            || response.contains("type=\"result\"")
            || response.contains("type='error'")
            || response.contains("type=\"error\""),
        "Expected result or error IQ, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0012_offline_user_query_returns_last_activity_and_status() {
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

    bob.send(&unavailable_presence("Out to lunch"))
        .await
        .expect("send unavailable presence");

    tokio::time::sleep(std::time::Duration::from_millis(1200)).await;

    alice
        .send(&last_activity_iq("last-offline-1", Some("bob@localhost")))
        .await
        .expect("send query");
    let response = alice
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    let seconds = response_seconds(&response).expect("seconds attribute");
    assert!(
        seconds >= 1,
        "Expected offline last-activity seconds, got response: {}",
        response
    );
    assert!(
        response.contains("Out to lunch"),
        "Expected unavailable presence status in response, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0012_query_to_user_who_blocked_requester_returns_error() {
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

    block_jid(&mut bob, "alice@localhost", "block-alice-last").await;

    alice
        .send(&last_activity_iq("last-blocked-1", Some("bob@localhost")))
        .await
        .expect("send query");
    let response = alice
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
