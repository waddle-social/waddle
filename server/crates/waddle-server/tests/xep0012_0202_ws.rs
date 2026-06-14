//! XEP-0012 last activity and XEP-0202 entity time over WebSocket C2S.

mod ws_common;

use std::str::FromStr;
use std::time::Duration;

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

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_last_activity_query_returns_server_uptime() {
    let (_server, mut client) = setup().await;

    let response = last_activity_query(&mut client, DOMAIN, "ws-last-1")
        .await
        .expect("last activity response");

    assert!(
        response.contains("type='result'") || response.contains("type='result'"),
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

    let _ = client.close().await;
}

#[tokio::test]
async fn websocket_entity_time_query_returns_utc_and_tzo() {
    let (_server, mut client) = setup().await;

    let response = entity_time_query(&mut client, DOMAIN, "ws-time-1")
        .await
        .expect("entity time response");

    assert!(
        response.contains("type='result'") || response.contains("type='result'"),
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

    let _ = client.close().await;
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
        response.contains("type='error'") || response.contains("type='error'"),
        "expected error IQ, got: {response}"
    );
    assert!(
        response.contains("bad-request"),
        "expected bad-request for invalid entity time IQ, got: {response}"
    );

    let _ = client.close().await;
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
        response.contains("type='error'") || response.contains("type='error'"),
        "expected error IQ, got: {response}"
    );
    assert!(
        response.contains("service-unavailable"),
        "expected service-unavailable for non-server entity time target, got: {response}"
    );

    let _ = client.close().await;
}

/// Insert an OIDC-provisioned identity straight into `users`, mirroring
/// `auth/identity.rs::create_user`. The ws harness only provisions native
/// accounts, so this is the only way to exercise the OIDC user path.
async fn seed_oidc_user(database_url: &str, localpart: &str) {
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    let options = SqliteConnectOptions::from_str(database_url)
        .expect("parse sqlite url")
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("open sqlite db for oidc seed");
    sqlx::query(
        "INSERT INTO users \
         (id, username, xmpp_localpart, display_name, avatar_url, primary_email, created_at, updated_at) \
         VALUES (?, ?, ?, ?, NULL, NULL, ?, ?)",
    )
    .bind(format!("id-{localpart}"))
    .bind(localpart)
    .bind(localpart)
    .bind("OIDC Member")
    .bind("2026-01-01T00:00:00Z")
    .bind("2026-01-01T00:00:00Z")
    .execute(&pool)
    .await
    .expect("seed oidc user row");
    pool.close().await;
}

/// Regression: a `jabber:iq:last` query for an offline **OIDC-registered** user
/// must return `forbidden` (a known account whose presence is private), not
/// `service-unavailable` (which means "no such entity"). The existence gate
/// previously consulted `native_users` only, so OIDC users were reported as
/// nonexistent. See issue #983.
#[tokio::test]
async fn websocket_last_activity_for_offline_oidc_user_is_forbidden() {
    let db_dir = tempfile::tempdir().expect("temp db dir");
    let db_path = db_dir.path().join("xep0012-oidc.sqlite3");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let server = TestServer::start_persistent_with_extra_accounts(&database_url, &[]);
    let password = server.fixed_account_password().to_string();
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        USERNAME,
        &password,
        &format!("activity-oidc-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("websocket connection");

    // `carol` exists only via OIDC (a row in `users`), is offline, and has no
    // recorded activity — so the query falls through to the existence gate.
    seed_oidc_user(&database_url, "carol").await;

    let response = last_activity_query(&mut client, "carol@localhost", "ws-last-oidc")
        .await
        .expect("last activity response");
    assert!(
        response.contains("forbidden"),
        "offline OIDC user must be forbidden (known account), got: {response}"
    );
    assert!(
        !response.contains("service-unavailable"),
        "OIDC user must not be reported as nonexistent: {response}"
    );

    // Control: a truly unknown local user is still service-unavailable.
    let ghost = last_activity_query(&mut client, "ghost@localhost", "ws-last-ghost")
        .await
        .expect("ghost response");
    assert!(
        ghost.contains("service-unavailable"),
        "unknown local user must be service-unavailable: {ghost}"
    );

    let _ = client.close().await;
}
