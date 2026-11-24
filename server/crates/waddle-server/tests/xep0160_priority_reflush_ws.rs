//! XEP-0160 §3 step 5 over WebSocket C2S: "When the recipient next sends
//! non-negative available presence to the server, the server delivers the
//! message to the resource that has sent that presence." RFC 6121
//! §8.5.2.1.1: a resource with negative priority never receives a message
//! addressed to the bare JID, so one sent meanwhile is stored offline and
//! must be delivered when the same session returns to non-negative
//! priority, even though that session already flushed once.
#![cfg(feature = "test-support")]

use waddle_ws_test_support as ws_common;

use std::time::Duration;

use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";

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

fn presence(priority: i8) -> String {
    format!(r#"<presence xmlns="jabber:client"><priority>{priority}</priority></presence>"#)
}

#[tokio::test]
async fn message_stored_during_negative_priority_is_delivered_on_return_to_non_negative() {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);
    let mut alice = connect_user(&server, "alice", &alice_password).await;
    let mut bob = connect_user(&server, "bob", &bob_password).await;

    // The session's first non-negative presence consumes its flush.
    alice.send(&presence(1)).await.expect("alice available");
    tokio::time::sleep(Duration::from_millis(300)).await;

    // Negative priority: nothing may be delivered to the bare JID.
    alice.send(&presence(-1)).await.expect("alice negative");
    tokio::time::sleep(Duration::from_millis(300)).await;
    bob.send(
        r#"<message xmlns="jabber:client" type="chat" to="alice@localhost" id="neg-1"><body>while negative</body></message>"#,
    )
    .await
    .expect("bob sends");
    let early = alice
        .recv_matching_within(Duration::from_millis(1500), |frame| {
            frame.contains("while negative")
        })
        .await;
    assert!(
        early.is_err(),
        "a negative-priority resource must not receive a bare-JID message: {early:?}"
    );

    // Back to non-negative priority: the stored message is delivered with
    // a delay stamp (XEP-0203).
    alice
        .send(&presence(1))
        .await
        .expect("alice available again");
    let delivered = alice
        .recv_matching(|frame| frame.contains("while negative"))
        .await
        .expect("stored message delivered on return to non-negative priority");
    assert!(
        delivered.contains("urn:xmpp:delay"),
        "delivery must carry a delay stamp: {delivered}"
    );

    let _ = alice.close().await;
    let _ = bob.close().await;
}
