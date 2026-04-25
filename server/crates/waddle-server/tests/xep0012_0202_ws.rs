//! XEP-0012 last activity and XEP-0202 entity time over WebSocket C2S.

mod ws_common;

use ws_common::{
    disco_info_query, entity_time_query, extract_element_text, last_activity_query, TestServer,
    WsXmppClient,
};

const DOMAIN: &str = "localhost";
const USERNAME: &str = "admin";

async fn setup() -> (TestServer, WsXmppClient) {
    let server = TestServer::start();
    let password = server.fixed_account_password().to_string();
    let client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("activity-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("websocket connection");
    (server, client)
}

#[tokio::test]
async fn websocket_disco_advertises_last_activity_and_entity_time() {
    let (_server, mut client) = setup().await;

    let response = disco_info_query(&mut client, DOMAIN, "ws-activity-disco")
        .await
        .expect("disco response");

    assert!(
        response.contains("jabber:iq:last"),
        "expected last activity feature, got: {response}"
    );
    assert!(
        response.contains("urn:xmpp:time"),
        "expected entity time feature, got: {response}"
    );

    client.close().await;
}

#[tokio::test]
async fn websocket_last_activity_query_returns_server_uptime() {
    let (_server, mut client) = setup().await;

    let response = last_activity_query(&mut client, DOMAIN, "ws-last-1")
        .await
        .expect("last activity response");

    assert!(
        response.contains("type=\"result\"") || response.contains("type='result'"),
        "expected result IQ, got: {response}"
    );
    assert!(
        response.contains("seconds="),
        "expected seconds attribute, got: {response}"
    );
    assert!(
        response.contains("ws-last-1"),
        "expected stanza id preserved, got: {response}"
    );

    client.close().await;
}

#[tokio::test]
async fn websocket_entity_time_query_returns_utc_and_tzo() {
    let (_server, mut client) = setup().await;

    let response = entity_time_query(&mut client, DOMAIN, "ws-time-1")
        .await
        .expect("entity time response");

    assert!(
        response.contains("type=\"result\"") || response.contains("type='result'"),
        "expected result IQ, got: {response}"
    );
    assert!(
        extract_element_text(&response, "utc").is_some(),
        "expected <utc/> child, got: {response}"
    );
    assert!(
        extract_element_text(&response, "tzo").is_some(),
        "expected <tzo/> child, got: {response}"
    );

    client.close().await;
}

#[tokio::test]
async fn websocket_entity_time_rejects_non_get_iq() {
    let (_server, mut client) = setup().await;

    client
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-time-set" to="localhost"><time xmlns="urn:xmpp:time"/></iq>"#,
        )
        .await
        .expect("send invalid entity time request");
    let response = client
        .recv_matching(|frame| frame.contains("ws-time-set"))
        .await
        .expect("entity time error response");

    assert!(
        response.contains("type=\"error\"") || response.contains("type='error'"),
        "expected error IQ, got: {response}"
    );
    assert!(
        response.contains("bad-request"),
        "expected bad-request for invalid entity time IQ, got: {response}"
    );

    client.close().await;
}

#[tokio::test]
async fn websocket_entity_time_rejects_non_server_target() {
    let (_server, mut client) = setup().await;

    client
        .send(
            r#"<iq xmlns="jabber:client" type="get" id="ws-time-user" to="admin@localhost"><time xmlns="urn:xmpp:time"/></iq>"#,
        )
        .await
        .expect("send user-targeted entity time request");
    let response = client
        .recv_matching(|frame| frame.contains("ws-time-user"))
        .await
        .expect("entity time error response");

    assert!(
        response.contains("type=\"error\"") || response.contains("type='error'"),
        "expected error IQ, got: {response}"
    );
    assert!(
        response.contains("service-unavailable"),
        "expected service-unavailable for non-server entity time target, got: {response}"
    );

    client.close().await;
}
