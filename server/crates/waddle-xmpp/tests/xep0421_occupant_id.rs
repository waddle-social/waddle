//! XEP-0421: Anonymous unique occupant identifiers compatibility suite.

mod common;

use common::{
    disco_info_query, establish_bound_session, init_test_env, join_muc_room, RawXmppClient,
    TestServer,
};

const OCCUPANT_ID_FEATURE: &str = "urn:xmpp:occupant-id:0";

#[tokio::test]
async fn xep0421_muc_room_advertises_occupant_id_feature() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0421", "desktop")
        .await
        .expect("bind session");

    join_muc_room(&mut client, "xep0421@muc.localhost", "RoomNick")
        .await
        .expect("join room");

    let response = disco_info_query(&mut client, "xep0421@muc.localhost", "xep0421-room-disco")
        .await
        .expect("room disco#info response");

    assert!(
        response.contains("var='urn:xmpp:occupant-id:0'")
            || response.contains("var=\"urn:xmpp:occupant-id:0\""),
        "Expected occupant-id feature in room disco#info, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0421_server_root_disco_does_not_advertise_occupant_id() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0421neg", "desktop")
        .await
        .expect("bind session");

    let response = disco_info_query(&mut client, "localhost", "xep0421-server-disco")
        .await
        .expect("server disco#info response");

    assert!(
        !response.contains(OCCUPANT_ID_FEATURE),
        "Server root disco should not advertise occupant-id, got: {}",
        response
    );
}

#[tokio::test]
async fn xep0421_muc_component_disco_without_room_does_not_advertise_occupant_id() {
    init_test_env();

    let server = TestServer::start().await;
    let mut client = RawXmppClient::connect(server.addr).await.expect("connect");
    establish_bound_session(&mut client, &server, "xep0421scope", "desktop")
        .await
        .expect("bind session");

    let response = disco_info_query(&mut client, "muc.localhost", "xep0421-muc-service")
        .await
        .expect("muc service disco#info response");

    assert!(
        !response.contains(OCCUPANT_ID_FEATURE),
        "MUC service disco should not advertise room-only occupant-id feature, got: {}",
        response
    );
}
