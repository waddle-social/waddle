//! RFC 6121 roster management behavior over the active WebSocket C2S transport.

mod ws_common;

use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";

async fn connect_alice_bob() -> (TestServer, WsXmppClient, WsXmppClient) {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);

    let alice = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("alice-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("alice connection");

    let bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");

    (server, alice, bob)
}

async fn roster_get(client: &mut WsXmppClient, id: &str) -> String {
    client
        .send(&format!(
            r#"<iq xmlns='jabber:client' type='get' id='{id}'><query xmlns='jabber:iq:roster'/></iq>"#
        ))
        .await
        .expect("send roster get");

    client
        .recv_matching(|frame| {
            frame.contains(&format!("id=\"{id}\"")) || frame.contains(&format!("id='{id}'"))
        })
        .await
        .expect("roster get response")
}

async fn roster_set(client: &mut WsXmppClient, id: &str, item_xml: &str) -> String {
    client
        .send(&format!(
            r#"<iq xmlns='jabber:client' type='set' id='{id}'><query xmlns='jabber:iq:roster'>{item_xml}</query></iq>"#
        ))
        .await
        .expect("send roster set");

    client
        .recv_matching(|frame| {
            frame.contains(&format!("id=\"{id}\"")) || frame.contains(&format!("id='{id}'"))
        })
        .await
        .expect("roster set result")
}

#[tokio::test]
async fn websocket_roster_get_add_update_remove_roundtrip() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    let add_result = roster_set(
        &mut alice,
        "roster-add-1",
        "<item jid='bob@localhost' name='Bob Friend'><group>Buddies</group></item>",
    )
    .await;
    assert!(add_result.contains("type=\"result\"") || add_result.contains("type='result'"));

    let get_after_add = roster_get(&mut alice, "roster-get-after-add").await;
    assert!(get_after_add.contains("<query") && get_after_add.contains("jabber:iq:roster"));
    assert!(
        get_after_add.contains("jid=\"bob@localhost\"")
            || get_after_add.contains("jid='bob@localhost'")
    );
    assert!(
        get_after_add.contains("name=\"Bob Friend\"")
            || get_after_add.contains("name='Bob Friend'")
    );
    assert!(get_after_add.contains("<group>Buddies</group>"));

    let update_result = roster_set(
        &mut alice,
        "roster-update-1",
        "<item jid='bob@localhost' name='Bobby'><group>Colleagues</group></item>",
    )
    .await;
    assert!(update_result.contains("type=\"result\"") || update_result.contains("type='result'"));

    let get_after_update = roster_get(&mut alice, "roster-get-after-update").await;
    assert!(
        get_after_update.contains("name=\"Bobby\"") || get_after_update.contains("name='Bobby'")
    );
    assert!(get_after_update.contains("<group>Colleagues</group>"));
    assert!(!get_after_update.contains("Bob Friend"));

    let remove_result = roster_set(
        &mut alice,
        "roster-remove-1",
        "<item jid='bob@localhost' subscription='remove' />",
    )
    .await;
    assert!(remove_result.contains("type=\"result\"") || remove_result.contains("type='result'"));

    let get_after_remove = roster_get(&mut alice, "roster-get-after-remove").await;
    assert!(!get_after_remove.contains("bob@localhost"));

    alice.close().await;
    bob.close().await;
}

#[tokio::test]
async fn websocket_roster_set_pushes_to_all_connected_resources() {
    let alice_password = format!("alice-pass-{}", uuid::Uuid::new_v4());
    let bob_password = format!("bob-pass-{}", uuid::Uuid::new_v4());
    let server = TestServer::start_with_extra_accounts(&[
        ("alice", &alice_password),
        ("bob", &bob_password),
    ]);

    let mut alice_one = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("alice-a-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("alice one connection");

    let mut alice_two = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "alice",
        &alice_password,
        &format!("alice-b-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("alice two connection");

    let mut bob = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "bob",
        &bob_password,
        &format!("bob-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("bob connection");

    let _result = roster_set(
        &mut alice_one,
        "roster-push-source",
        "<item jid='bob@localhost' name='Bob Push'/>",
    )
    .await;

    let push_one = alice_one
        .recv_matching(|frame| {
            frame.contains("type=\"set\"")
                && frame.contains("jabber:iq:roster")
                && frame.contains("bob@localhost")
        })
        .await
        .expect("requesting resource receives roster push");
    assert!(push_one.contains("jabber:iq:roster"));

    let push_two = alice_two
        .recv_matching(|frame| {
            frame.contains("type=\"set\"")
                && frame.contains("jabber:iq:roster")
                && frame.contains("bob@localhost")
        })
        .await
        .expect("second resource receives roster push");
    assert!(push_two.contains("jabber:iq:roster"));

    alice_one.close().await;
    alice_two.close().await;
    bob.close().await;
}

#[tokio::test]
async fn websocket_roster_reflects_presence_subscription_state() {
    let (_server, mut alice, mut bob) = connect_alice_bob().await;

    bob.send(r#"<presence xmlns='jabber:client' type='subscribe' to='alice@localhost'/>"#)
        .await
        .expect("send subscribe");
    let _subscribe_forward = alice
        .recv_matching(|frame| {
            frame.contains("type=\"subscribe\"") || frame.contains("type='subscribe'")
        })
        .await
        .expect("alice receives subscribe");

    alice
        .send(r#"<presence xmlns='jabber:client' type='subscribed' to='bob@localhost'/>"#)
        .await
        .expect("send subscribed");
    let _subscribed_forward = bob
        .recv_matching(|frame| {
            frame.contains("type=\"subscribed\"") || frame.contains("type='subscribed'")
        })
        .await
        .expect("bob receives subscribed");

    let alice_roster = roster_get(&mut alice, "roster-sub-state").await;
    assert!(
        alice_roster.contains("jid=\"bob@localhost\"")
            || alice_roster.contains("jid='bob@localhost'")
    );
    assert!(
        alice_roster.contains("subscription=\"to\"") || alice_roster.contains("subscription='to'")
    );

    bob.close().await;
    alice.close().await;
}
