//! XEP-0410: MUC Self-Ping Optimization dedicated suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, join_muc_room, ping_query,
    RawXmppClient, TestServer,
};

#[tokio::test]
async fn xep0410_self_ping_succeeds_for_own_occupant() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0410", "desktop")
        .await
        .expect("bind session");

    join_muc_room(&mut client, "selfping@muc.localhost", "SelfNick")
        .await
        .expect("join room");

    let response = ping_query(
        &mut client,
        "selfping@muc.localhost/SelfNick",
        "xep0410-self-ping",
    )
    .await
    .expect("self ping response");

    assert!(
        response.contains("type='result'") || response.contains("type=\"result\""),
        "Expected successful self-ping result, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0410_ping_to_unknown_occupant_returns_item_not_found() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0410neg", "desktop")
        .await
        .expect("bind session");

    join_muc_room(&mut client, "missing@muc.localhost", "ExistingNick")
        .await
        .expect("join room");

    let response = ping_query(
        &mut client,
        "missing@muc.localhost/NoSuchNick",
        "xep0410-missing",
    )
    .await
    .expect("missing occupant ping response");

    assert!(
        response.contains("type='error'") || response.contains("type=\"error\""),
        "Expected error for unknown occupant self-ping, got: {}",
        response
    );
    assert!(
        response.contains("item-not-found"),
        "Expected item-not-found for unknown occupant self-ping, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0410_muc_service_advertises_self_ping_optimization_feature() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0410disco", "desktop")
        .await
        .expect("bind session");

    let response = disco_info_query(&mut client, "muc.localhost", "xep0410-disco")
        .await
        .expect("muc disco#info response");

    assert!(
        response.contains("var='urn:xmpp:muc-selfping:0'")
            || response.contains("var=\"urn:xmpp:muc-selfping:0\""),
        "Expected MUC self-ping optimization feature in MUC disco#info, got: {}",
        response
    );
}
