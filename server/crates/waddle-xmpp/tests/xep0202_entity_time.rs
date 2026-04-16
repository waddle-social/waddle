#![recursion_limit = "256"]

//! XEP-0202: Entity Time dedicated integration suite.
//!
//! Note: The server implements XEP-0202 parsing/building helpers but may not
//! advertise or handle time queries at the server level. These tests validate
//! the server's actual behavior.

mod common;

use common::{establish_bound_session, init_test_env, RawXmppClient, TestServer, DEFAULT_TIMEOUT};

#[tokio::test]
async fn xep0202_time_query_to_server_returns_response() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='time-1' to='localhost' xmlns='jabber:client'>\
                <time xmlns='urn:xmpp:time'/>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    // Server must respond (result with time data or error)
    assert!(
        response.contains("type='result'")
            || response.contains("type=\"result\"")
            || response.contains("type='error'")
            || response.contains("type=\"error\""),
        "Expected result or error IQ, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0202_time_set_returns_error() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='set' id='time-bad-1' to='localhost' xmlns='jabber:client'>\
                <time xmlns='urn:xmpp:time'/>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    assert!(
        response.contains("type='error'") || response.contains("type=\"error\""),
        "Expected error for set on time, got: {}",
        response
    );
}
