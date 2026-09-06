//! Graceful shutdown of live WebSocket sessions (issue #1091).
//!
//! On SIGTERM the server must natively close every live session:
//! send `<stream:error><system-shutdown/>` (RFC 6120 §4.9.3.20)
//! followed by the RFC 7395 `<close/>` frame, detach XEP-0198
//! sessions so their unacked queues flow through Q6 promotion, and
//! exit within the drain timeout — no SIGKILL required.

use std::time::Duration;

use waddle_ws_test_support as ws_common;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";

/// SIGTERM with a live authenticated client: the client receives
/// `system-shutdown` then the framing `<close/>`, and the process
/// exits well within the drain timeout instead of hanging until
/// SIGKILL.
#[tokio::test]
async fn sigterm_closes_live_session_with_system_shutdown_and_exits() {
    let mut server = TestServer::start_with_extra_envs(&[], &[("WADDLE_DRAIN_TIMEOUT_SECS", "5")]);
    let mut client = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "shutdown-test",
    )
    .await
    .expect("connect and auth");

    server.send_sigterm();

    let stream_error = client
        .recv_matching(|frame| frame.contains("<stream:error"))
        .await
        .expect("receive stream error after SIGTERM");
    assert!(
        stream_error.contains("system-shutdown"),
        "stream error must carry RFC 6120 §4.9.3.20 system-shutdown: {stream_error}"
    );
    assert!(
        stream_error.contains("urn:ietf:params:xml:ns:xmpp-streams"),
        "system-shutdown must be in the xmpp-streams namespace: {stream_error}"
    );

    let close = client
        .recv_matching(|frame| frame.contains("<close"))
        .await
        .expect("receive RFC 7395 close frame after stream error");
    assert!(
        close.contains("urn:ietf:params:xml:ns:xmpp-framing"),
        "close frame must use the xmpp-framing namespace: {close}"
    );

    assert!(
        server.wait_for_exit(Duration::from_secs(20)).await,
        "server must exit on its own within the drain timeout"
    );
}

/// A live XEP-0198 session's unacked stanzas must survive SIGTERM:
/// the session detaches on shutdown, the drain promotes its unacked
/// queue (Q6) into durable offline delivery, and the recipient gets
/// the message after the server restarts — instead of losing it on
/// every deploy.
#[tokio::test]
async fn sigterm_promotes_live_sm_session_unacked_queue_for_next_startup_delivery() {
    // RAII tempdir: cleans up the sqlite files even when an assertion
    // below panics; the guard outlives both server phases.
    let scratch_dir = tempfile::tempdir().expect("create scratch dir");
    let scratch = scratch_dir.path();
    let global_db = format!("sqlite://{}?mode=rwc", scratch.join("global.db").display());
    let sm_db = format!("sqlite://{}?mode=rwc", scratch.join("sm.db").display());
    let pending_db = format!("sqlite://{}?mode=rwc", scratch.join("pending.db").display());
    // MAM and inbox inherit the durable global database: RFC 0018 §4
    // requires their writes to share the ingress transaction. The promoted
    // pending_delivery row resolves its archived stanza there after restart;
    // SM and pending delivery retain their separate persistent stores.
    let extra_envs: Vec<(&str, &str)> = vec![
        ("WADDLE_XMPP_SM_DATABASE_URL", sm_db.as_str()),
        (
            "WADDLE_XMPP_PENDING_DELIVERY_DATABASE_URL",
            pending_db.as_str(),
        ),
        ("WADDLE_DRAIN_TIMEOUT_SECS", "5"),
    ];

    // Phase 1: admin holds a live resumable SM session with one
    // unacked inbound DM when SIGTERM lands.
    let body_text = "unacked-message-must-survive-shutdown";
    {
        let mut server = TestServer::start_persistent_with_extra_envs(
            &global_db,
            &[("bob", "bob-shutdown-password-1")],
            &extra_envs,
        );
        let mut admin = WsXmppClient::connect_and_auth(
            &server.ws_url(),
            DOMAIN,
            "admin",
            server.fixed_account_password(),
            "shutdown-q6",
        )
        .await
        .expect("connect admin");
        admin
            .send(r#"<enable xmlns="urn:xmpp:sm:3" resume="true"/>"#)
            .await
            .expect("send SM enable");
        admin
            .recv_matching(|frame| frame.contains("<enabled"))
            .await
            .expect("SM enabled");

        let mut bob = WsXmppClient::connect_and_auth(
            &server.ws_url(),
            DOMAIN,
            "bob",
            "bob-shutdown-password-1",
            "shutdown-bob",
        )
        .await
        .expect("connect bob");
        let admin_jid = admin.full_jid.clone().expect("admin bound jid");
        bob.send(&format!(
            r#"<message to="{admin_jid}" type="chat" id="q6-1"><body xmlns="jabber:client">{body_text}</body></message>"#
        ))
        .await
        .expect("bob sends DM");
        // The DM reaches admin's live session (now in the unacked
        // queue — admin never sends <a/>).
        admin
            .recv_matching(|frame| frame.contains(body_text))
            .await
            .expect("admin receives DM before shutdown");

        server.send_sigterm();
        admin
            .recv_matching(|frame| frame.contains("system-shutdown"))
            .await
            .expect("admin sees system-shutdown");
        assert!(
            server.wait_for_exit(Duration::from_secs(20)).await,
            "server must exit after promoting unacked queues"
        );
    }

    // Phase 2: fresh process on the same databases — the promoted
    // stanza is delivered as offline mail on initial presence.
    let server = TestServer::start_persistent_with_extra_envs(
        &global_db,
        &[("bob", "bob-shutdown-password-1")],
        &extra_envs,
    );
    let mut admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "shutdown-q6-return",
    )
    .await
    .expect("reconnect admin");
    admin.send("<presence/>").await.expect("initial presence");
    let delivered = admin
        .recv_matching(|frame| frame.contains(body_text))
        .await
        .expect("promoted unacked DM delivered after restart");
    assert!(
        delivered.contains("urn:xmpp:delay"),
        "offline redelivery must carry an XEP-0203 delay stamp: {delivered}"
    );
}

/// SIGTERM with several live clients: every session gets the
/// system-shutdown stream error, not just one.
#[tokio::test]
async fn sigterm_notifies_every_live_session() {
    let mut server = TestServer::start_with_extra_envs(
        &[("bob", "bob-shutdown-password-1")],
        &[("WADDLE_DRAIN_TIMEOUT_SECS", "5")],
    );
    let mut admin = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "shutdown-admin",
    )
    .await
    .expect("connect admin");
    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        "bob-shutdown-password-1",
        "shutdown-bob",
    )
    .await
    .expect("connect bob");

    server.send_sigterm();

    for client in [&mut admin, &mut bob] {
        let stream_error = client
            .recv_matching(|frame| frame.contains("<stream:error"))
            .await
            .expect("each live session receives a stream error");
        assert!(
            stream_error.contains("system-shutdown"),
            "expected system-shutdown, got: {stream_error}"
        );
    }

    assert!(
        server.wait_for_exit(Duration::from_secs(20)).await,
        "server must exit with multiple clients connected"
    );
}
