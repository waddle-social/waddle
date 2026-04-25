//! XEP-0092 software version over the active WebSocket C2S transport.

mod ws_common;

use ws_common::{disco_info_query, extract_element_text, version_query, TestServer, WsXmppClient};

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
        &format!("version-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("websocket connection");
    (server, client)
}

#[tokio::test]
async fn websocket_disco_advertises_software_version() {
    let (_server, mut client) = setup().await;

    let response = disco_info_query(&mut client, DOMAIN, "ws-version-disco")
        .await
        .expect("disco response");

    assert!(
        response.contains("jabber:iq:version"),
        "expected software version feature, got: {response}"
    );

    client.close().await;
}

#[tokio::test]
async fn websocket_version_query_returns_name_version_and_os() {
    let (_server, mut client) = setup().await;

    let response = version_query(&mut client, DOMAIN, "ws-version-1")
        .await
        .expect("version response");

    assert!(
        response.contains("type=\"result\"") || response.contains("type='result'"),
        "expected result IQ, got: {response}"
    );
    assert_eq!(
        extract_element_text(&response, "name").as_deref(),
        Some("Waddle")
    );
    assert!(
        extract_element_text(&response, "version").is_some(),
        "expected <version/> child, got: {response}"
    );
    assert!(
        extract_element_text(&response, "os").is_some(),
        "expected <os/> child, got: {response}"
    );
    assert!(
        response.contains("ws-version-1"),
        "expected stanza id preserved, got: {response}"
    );

    client.close().await;
}

#[tokio::test]
async fn websocket_version_rejects_non_get_iq() {
    let (_server, mut client) = setup().await;

    client
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-version-set" to="localhost"><query xmlns="jabber:iq:version"/></iq>"#,
        )
        .await
        .expect("send invalid version request");
    let response = client
        .recv_matching(|frame| frame.contains("ws-version-set"))
        .await
        .expect("version error response");

    assert!(
        response.contains("type=\"error\"") || response.contains("type='error'"),
        "expected error IQ, got: {response}"
    );
    assert!(
        response.contains("bad-request"),
        "expected bad-request for invalid version IQ, got: {response}"
    );

    client.close().await;
}

#[tokio::test]
async fn websocket_version_rejects_non_server_target() {
    let (_server, mut client) = setup().await;

    client
        .send(
            r#"<iq xmlns="jabber:client" type="get" id="ws-version-user" to="admin@localhost"><query xmlns="jabber:iq:version"/></iq>"#,
        )
        .await
        .expect("send user-targeted version request");
    let response = client
        .recv_matching(|frame| frame.contains("ws-version-user"))
        .await
        .expect("version error response");

    assert!(
        response.contains("type=\"error\"") || response.contains("type='error'"),
        "expected error IQ, got: {response}"
    );
    assert!(
        response.contains("service-unavailable"),
        "expected service-unavailable for non-server version target, got: {response}"
    );

    client.close().await;
}
