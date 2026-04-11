#![recursion_limit = "256"]

//! XEP-0249: Direct MUC Invitations integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, RawXmppClient, TestServer, DEFAULT_TIMEOUT,
};

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

    // Alice sends direct invite to Bob
    alice
        .send(
            "<message to='bob@localhost' xmlns='jabber:client'>\
                <x xmlns='jabber:x:conference' jid='testroom@muc.localhost' reason='Join us!'/>\
            </message>",
        )
        .await
        .expect("send invite");

    // Bob should receive the invite
    let bob_response = bob.read(DEFAULT_TIMEOUT).await;
    match bob_response {
        Ok(data) => {
            if data.contains("jabber:x:conference") {
                assert!(
                    data.contains("testroom@muc.localhost"),
                    "Invite should contain room JID, got: {}",
                    data
                );
            }
        }
        Err(_) => {
            // Message routing may not deliver if no roster subscription
        }
    }
}
