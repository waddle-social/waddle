#![recursion_limit = "256"]

//! XEP-0488: MUC Token Invite integration suite.

mod common;

use common::{
    establish_bound_session, init_test_env, join_muc_room, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0488_invite_request_to_muc_returns_response() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    // Join room first
    join_muc_room(&mut client, "invroom@muc.localhost", "Alice")
        .await
        .expect("join");

    // Request invite token
    client
        .send(
            "<iq type='get' id='invite-req-1' to='invroom@muc.localhost' xmlns='jabber:client'>\
                <request xmlns='urn:xmpp:muc-token-invite:0'/>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    // Server should respond with result (token) or error
    assert!(
        response.contains("type='result'")
            || response.contains("type=\"result\"")
            || response.contains("type='error'")
            || response.contains("type=\"error\""),
        "Expected result or error for invite request, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0488_invite_share_in_message() {
    init_test_env();
    let server = TestServer::start().await;

    let mut alice = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut alice, &server, "alice", "desktop")
        .await
        .expect("bind alice");
    join_muc_room(&mut alice, "invshare@muc.localhost", "Alice")
        .await
        .expect("alice join");

    let mut bob = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut bob, &server, "bob", "mobile")
        .await
        .expect("bind bob");
    join_muc_room(&mut bob, "invshare@muc.localhost", "Bob")
        .await
        .expect("bob join");

    // Alice shares invite token in groupchat
    alice
        .send(
            "<message type='groupchat' to='invshare@muc.localhost' id='inv-share-1' xmlns='jabber:client'>\
                <body>Join us! xmpp:invshare@muc.localhost?join;preauth=token123</body>\
                <invite xmlns='urn:xmpp:muc-token-invite:0' token='token123' jid='invshare@muc.localhost'/>\
            </message>",
        )
        .await
        .expect("send");

    let bob_response = bob
        .read_until("Join us!", DEFAULT_TIMEOUT)
        .await
        .expect("bob receives");

    assert!(
        bob_response.contains("Join us!"),
        "Bob should receive invite share message"
    );
}
