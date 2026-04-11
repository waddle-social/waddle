#![recursion_limit = "256"]

//! XEP-0050: Ad-Hoc Commands dedicated integration suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, RawXmppClient, TestServer,
    DEFAULT_TIMEOUT,
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
        "Expected commands feature, got: {}",
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

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected result IQ, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0050_execute_unknown_command_returns_item_not_found() {
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
        response.contains("item-not-found"),
        "Expected item-not-found, got: {}",
        response
    );
}
