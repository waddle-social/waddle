//! XEP-0012 last activity and XEP-0202 entity time over WebSocket C2S.

use waddle_ws_test_support as ws_common;

use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};
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
         (jid, username, xmpp_localpart, localpart_key, display_name, avatar_url, primary_email, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, NULL, NULL, ?, ?)",
    )
    .bind(format!("{localpart}@localhost"))
    .bind(localpart)
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

// ---------------------------------------------------------------------------
// XEP-0012 presence-authorization gate: last activity is only revealed to the
// account owner and to contacts holding a presence subscription (RFC 6121).
// ---------------------------------------------------------------------------

async fn connect_user(server: &TestServer, name: &str, password: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        name,
        password,
        &format!("{name}-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("websocket connection")
}

async fn start_alice_bob() -> (TestServer, WsXmppClient, WsXmppClient) {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let alice = connect_user(&server, "alice", &alice_password).await;
    let bob = connect_user(&server, "bob", &bob_password).await;
    (server, alice, bob)
}

/// Bob subscribes to alice and alice approves, so alice's roster holds bob as
/// `from` (bob may see alice's presence).
async fn bob_subscribes_to_alice(alice: &mut WsXmppClient, bob: &mut WsXmppClient) {
    for (client, id) in [(&mut *alice, "alice-roster"), (&mut *bob, "bob-roster")] {
        client
            .send(&format!(
                r#"<iq xmlns="jabber:client" type="get" id="{id}"><query xmlns="jabber:iq:roster"/></iq>"#
            ))
            .await
            .expect("send roster get");
        client
            .recv_matching(|frame| frame.contains(id))
            .await
            .expect("roster result");
    }
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice available");
    bob.send(r#"<presence xmlns="jabber:client" type="subscribe" to="alice@localhost"/>"#)
        .await
        .expect("bob subscribes");
    alice
        .recv_matching(|frame| frame.contains("type='subscribe'"))
        .await
        .expect("alice receives subscribe");
    alice
        .send(r#"<presence xmlns="jabber:client" type="subscribed" to="bob@localhost"/>"#)
        .await
        .expect("alice approves");
    bob.recv_matching(|frame| frame.contains("type='subscribed'"))
        .await
        .expect("bob receives approval");
}

#[tokio::test]
async fn websocket_last_activity_unsubscribed_requester_is_forbidden_online_and_offline() {
    let (_server, mut alice, mut bob) = start_alice_bob().await;
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice available");

    // Online target: must not reveal availability.
    let online = last_activity_query(&mut bob, "alice@localhost", "ws-last-unsub-online")
        .await
        .expect("online response");
    assert!(online.contains("type='error'"), "expected error: {online}");
    assert!(online.contains("forbidden"), "expected forbidden: {online}");
    assert!(
        !online.contains("jabber:iq:last") && !online.contains("seconds="),
        "unauthorized query must not leak last activity: {online}"
    );

    // Offline target: must not reveal logout time or status.
    alice
        .send(
            r#"<presence xmlns="jabber:client" type="unavailable"><status>secret status</status></presence>"#,
        )
        .await
        .expect("alice unavailable");
    let _ = alice.close().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let offline = last_activity_query(&mut bob, "alice@localhost", "ws-last-unsub-offline")
        .await
        .expect("offline response");
    assert!(
        offline.contains("forbidden"),
        "expected forbidden: {offline}"
    );
    assert!(
        !offline.contains("secret status")
            && !offline.contains("jabber:iq:last")
            && !offline.contains("seconds="),
        "unauthorized query must not leak last activity: {offline}"
    );

    let _ = bob.close().await;
}

#[tokio::test]
async fn websocket_last_activity_subscribed_contact_receives_activity() {
    let (_server, mut alice, mut bob) = start_alice_bob().await;
    bob_subscribes_to_alice(&mut alice, &mut bob).await;

    let online = last_activity_query(&mut bob, "alice@localhost", "ws-last-sub-online")
        .await
        .expect("online response");
    assert!(
        online.contains("type='result'"),
        "expected result: {online}"
    );
    assert!(
        online.contains("seconds='0'"),
        "expected seconds=0: {online}"
    );

    alice
        .send(
            r#"<presence xmlns="jabber:client" type="unavailable"><status>gone fishing</status></presence>"#,
        )
        .await
        .expect("alice unavailable");
    let _ = alice.close().await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let offline = last_activity_query(&mut bob, "alice@localhost", "ws-last-sub-offline")
        .await
        .expect("offline response");
    assert!(
        offline.contains("type='result'"),
        "expected result: {offline}"
    );
    assert!(offline.contains("seconds="), "expected seconds: {offline}");
    assert!(
        offline.contains("gone fishing"),
        "expected last status text: {offline}"
    );

    let _ = bob.close().await;
}

/// Start a persistent server holding alice and bob on `database_url`.
async fn start_persistent_alice_bob(
    database_url: &str,
    alice_password: &str,
    bob_password: &str,
) -> (TestServer, WsXmppClient, WsXmppClient) {
    let server = TestServer::start_persistent_with_extra_accounts(
        database_url,
        &[("alice", alice_password), ("bob", bob_password)],
    );
    let alice = connect_user(&server, "alice", alice_password).await;
    let bob = connect_user(&server, "bob", bob_password).await;
    (server, alice, bob)
}

/// XEP-0012 "Offline User Query": the last logout is durable, so an
/// authorized contact still gets "seconds" and the status text after the
/// server restarts (and from any other node sharing the database).
#[tokio::test]
async fn websocket_last_activity_survives_server_restart() {
    let db_dir = tempfile::tempdir().expect("temp db dir");
    let db_path = db_dir.path().join("xep0012-restart.sqlite3");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());

    let (server, mut alice, mut bob) =
        start_persistent_alice_bob(&database_url, &alice_password, &bob_password).await;
    bob_subscribes_to_alice(&mut alice, &mut bob).await;
    alice
        .send(
            r#"<presence xmlns="jabber:client" type="unavailable"><status>Heading Home</status></presence>"#,
        )
        .await
        .expect("alice unavailable");
    let _ = alice.close().await;
    let _ = bob.close().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    drop(server);

    let server = TestServer::start_persistent_with_extra_accounts(
        &database_url,
        &[("alice", &alice_password), ("bob", &bob_password)],
    );
    let mut bob = connect_user(&server, "bob", &bob_password).await;
    let response = last_activity_query(&mut bob, "alice@localhost", "ws-last-restart")
        .await
        .expect("last activity response");
    assert!(
        response.contains("type='result'"),
        "expected a result after restart: {response}"
    );
    assert!(
        response.contains("seconds="),
        "expected seconds: {response}"
    );
    assert!(
        response.contains("Heading Home"),
        "expected the last status text after restart: {response}"
    );
    let _ = bob.close().await;
}

/// An authorized requester is never told it is unauthorized: when the
/// account has no last-logout record the answer is `service-unavailable`
/// (there is no last activity to report), not `forbidden`.
#[tokio::test]
async fn websocket_last_activity_authorized_without_record_is_not_forbidden() {
    let db_dir = tempfile::tempdir().expect("temp db dir");
    let db_path = db_dir.path().join("xep0012-norecord.sqlite3");
    let database_url = format!("sqlite://{}?mode=rwc", db_path.display());
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());

    let (_server, mut alice, mut bob) =
        start_persistent_alice_bob(&database_url, &alice_password, &bob_password).await;
    bob_subscribes_to_alice(&mut alice, &mut bob).await;
    alice
        .send(r#"<presence xmlns="jabber:client" type="unavailable"/>"#)
        .await
        .expect("alice unavailable");
    let _ = alice.close().await;
    tokio::time::sleep(Duration::from_millis(300)).await;

    let options = SqliteConnectOptions::from_str(&database_url)
        .expect("parse sqlite url")
        .busy_timeout(Duration::from_secs(5));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("open sqlite db");
    let deleted = sqlx::query("DELETE FROM account_last_activity WHERE account_jid = ?")
        .bind("alice@localhost")
        .execute(&pool)
        .await
        .expect("delete last activity record");
    assert_eq!(deleted.rows_affected(), 1, "logout must have been recorded");
    pool.close().await;

    let response = last_activity_query(&mut bob, "alice@localhost", "ws-last-norecord")
        .await
        .expect("last activity response");
    assert!(
        response.contains("service-unavailable"),
        "expected service-unavailable: {response}"
    );
    assert!(
        !response.contains("forbidden"),
        "an authorized requester must not get forbidden: {response}"
    );
    let _ = bob.close().await;
}

#[tokio::test]
async fn websocket_last_activity_self_query_is_authorized() {
    let (_server, mut alice, bob) = start_alice_bob().await;
    alice
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("alice available");

    let response = last_activity_query(&mut alice, "alice@localhost", "ws-last-self")
        .await
        .expect("self response");
    assert!(
        response.contains("type='result'"),
        "expected result: {response}"
    );
    assert!(
        response.contains("seconds="),
        "expected seconds: {response}"
    );

    let _ = alice.close().await;
    let _ = bob.close().await;
}

#[tokio::test]
async fn websocket_last_activity_blocked_requester_stays_service_unavailable() {
    let (_server, mut alice, mut bob) = start_alice_bob().await;
    bob_subscribes_to_alice(&mut alice, &mut bob).await;

    alice
        .send(
            r#"<iq xmlns="jabber:client" type="set" id="ws-last-block"><block xmlns="urn:xmpp:blocking"><item jid="bob@localhost"/></block></iq>"#,
        )
        .await
        .expect("alice blocks bob");
    alice
        .recv_matching(|frame| frame.contains("ws-last-block") && frame.contains("type='result'"))
        .await
        .expect("block result");

    let response = last_activity_query(&mut bob, "alice@localhost", "ws-last-blocked")
        .await
        .expect("blocked response");
    assert!(
        response.contains("service-unavailable"),
        "blocked requester must keep service-unavailable: {response}"
    );
    assert!(
        !response.contains("seconds="),
        "blocked requester must not see activity: {response}"
    );

    let _ = bob.close().await;
}
