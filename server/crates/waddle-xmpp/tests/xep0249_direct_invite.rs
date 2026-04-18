#![recursion_limit = "256"]

//! XEP-0249: Direct MUC Invitations integration suite.

mod common;

use common::{establish_bound_session, init_test_env, RawXmppClient, TestServer, DEFAULT_TIMEOUT};

#[tokio::test]
async fn xep0249_direct_invite_delivered_to_recipient() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    alice
        .send("<presence xmlns='jabber:client'/>")
        .await
        .expect("presence");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    bob.send("<presence xmlns='jabber:client'/>")
        .await
        .expect("presence");

    tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    alice.clear();
    bob.clear();

    // Alice sends direct invite to Bob
    alice
        .send(
            "<message to='bob@localhost' xmlns='jabber:client'>\
                <x xmlns='jabber:x:conference' jid='testroom@muc.localhost' reason='Join us!'/>\
            </message>",
        )
        .await
        .expect("send invite");

    let bob_response = bob
        .read_until("jabber:x:conference", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives invite");

    assert!(
        bob_response.contains("testroom@muc.localhost"),
        "Invite should contain room JID, got: {}",
        bob_response
    );
    assert!(
        bob_response.contains("reason='Join us!'") || bob_response.contains("reason=\"Join us!\""),
        "Invite should preserve the reason, got: {}",
        bob_response
    );
}
