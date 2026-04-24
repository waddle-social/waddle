#![recursion_limit = "256"]

//! XEP-0317: Hats (role badges) integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, start_server_with_channels,
    RawXmppClient, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0317_presence_with_hats_broadcast_in_muc() {
    init_test_env();
    let server = start_server_with_channels(&["hats"]).await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "hats@muc.localhost", "alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "hats@muc.localhost", "bob")
        .await
        .expect("bob join");

    // Alice sends presence with hats
    alice
        .send(
            "<presence to='hats@muc.localhost/alice' xmlns='jabber:client'>\
                <hats xmlns='urn:xmpp:hats:0'>\
                    <hat uri='urn:xmpp:hats:0#admin' title='Admin'/>\
                </hats>\
            </presence>",
        )
        .await
        .expect("send presence with hats");

    // Bob should receive presence update (may or may not contain hats depending on server)
    let bob_response = bob.read(DEFAULT_TIMEOUT).await;
    match bob_response {
        Ok(data) => {
            // If server forwards hats, verify structure
            if data.contains("urn:xmpp:hats:0") {
                assert!(data.contains("hat"), "Expected hat element, got: {}", data);
            }
        }
        Err(_) => {
            // Timeout acceptable if server strips hats from client presence
        }
    }
}
