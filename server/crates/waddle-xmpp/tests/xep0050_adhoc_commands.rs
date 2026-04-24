#![recursion_limit = "256"]

//! XEP-0050: Ad-Hoc Commands dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, start_server_with_channels,
    RawXmppClient, TestServer, DEFAULT_TIMEOUT,
};

#[tokio::test]
async fn xep0050_server_disco_advertises_commands() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    let response = disco_info_query(&mut client, "localhost", "cmd-disco-1")
        .await
        .expect("disco response");

    assert!(
        response.contains("http://jabber.org/protocol/commands"),
        "Server should advertise commands support, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0050_commands_disco_items_lists_available_commands() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='get' id='cmd-items-1' to='localhost' xmlns='jabber:client'>\
                <query xmlns='http://jabber.org/protocol/disco#items' \
                    node='http://jabber.org/protocol/commands'/>\
            </iq>",
        )
        .await
        .expect("send");
    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    // Should return result with available commands (even if the list is empty)
    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ for commands disco#items, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0050_execute_unknown_command_returns_service_unavailable() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    client
        .send(
            "<iq type='set' id='cmd-exec-1' to='localhost' xmlns='jabber:client'>\
                <command xmlns='http://jabber.org/protocol/commands' \
                    node='nonexistent-command' action='execute'/>\
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
        "Expected error for unknown command, got: {}",
        response
    );
    assert!(
        response.contains("service-unavailable"),
        "Expected service-unavailable, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0050_create_channel_command_prevents_managed_jid_instant_room() {
    init_test_env();
    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    // Try to join a managed channel JID that doesn't exist
    // This should be blocked to prevent bypassing the create-channel command
    client
        .send(
            "<presence to='test-channel@muc.localhost/alice' xmlns='jabber:client'>\
                <x xmlns='http://jabber.org/protocol/muc'/>\
            </presence>",
        )
        .await
        .expect("send");

    // Try to read a response - might get error presence or stream might close
    match client.read_until("</presence>", DEFAULT_TIMEOUT).await {
        Ok(response) => {
            // If we get a response, verify it's not a successful join
            assert!(
                !response.contains("<status code='110'/>")
                    && !response.contains("<status code=\"110\"/>"),
                "Should not successfully join managed JID without creating channel first, got: {}",
                response
            );
        }
        Err(_) => {
            // Timeout or closed stream is acceptable - connection might be terminated on error
        }
    }
}

#[tokio::test]
async fn xep0050_managed_jid_owner_query_blocked() {
    init_test_env();
    let server = start_server_with_channels(&["test-channel"]).await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "alice", "desktop")
        .await
        .expect("bind");

    // Drain auto-join self-presence
    let _ = client.read_until("</presence>", DEFAULT_TIMEOUT).await;
    client.clear();

    // Try to configure a managed channel JID via owner query
    // This should be blocked
    client
        .send(
            "<iq type='get' id='owner-1' to='test-channel@muc.localhost' xmlns='jabber:client'>\
                <query xmlns='http://jabber.org/protocol/muc#owner'/>\
            </iq>",
        )
        .await
        .expect("send");

    let response = client
        .read_until("</iq>", DEFAULT_TIMEOUT)
        .await
        .expect("response");

    // Should receive an error with not-allowed
    assert!(
        response.contains("type='error'") || response.contains("type=\"error\""),
        "Expected error IQ for managed JID owner query, got: {}",
        response
    );
    assert!(
        response.contains("not-allowed"),
        "Expected not-allowed for managed JID configuration, got: {}",
        response
    );
}
