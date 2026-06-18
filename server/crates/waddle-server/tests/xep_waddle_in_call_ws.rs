//! Waddle in-call signaling integration tests over WebSocket.

mod ws_common;

use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ALICE: &str = "admin";
const BOB: &str = "bob";
const BOB_PASSWORD: &str = "bob-password";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn connect(
    server: &TestServer,
    username: &str,
    password: &str,
    resource_prefix: &str,
) -> WsXmppClient {
    let resource = format!("{resource_prefix}-{}", uuid::Uuid::new_v4());
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, username, password, &resource)
        .await
        .expect("connect and auth")
}

async fn join_room(client: &mut WsXmppClient, room: &str, nick: &str) {
    client
        .send(&format!(
            r#"<presence to="{room}/{nick}"><x xmlns="http://jabber.org/protocol/muc"/></presence>"#
        ))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
}

#[tokio::test]
async fn direct_in_call_reaction_routes_to_peer_full_jid_and_is_not_archived() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[(BOB, BOB_PASSWORD)]);
    let alice_password = server.fixed_account_password().to_string();
    let mut alice = connect(&server, ALICE, &alice_password, "in-call-alice").await;
    let mut bob = connect(&server, BOB, BOB_PASSWORD, "in-call-bob").await;
    let bob_full = bob.full_jid.clone().expect("bob full jid");
    let alice_bare = format!("{ALICE}@{DOMAIN}");
    let bob_bare = format!("{BOB}@{DOMAIN}");

    alice
        .send(&format!(
            r#"<message type="chat" to="{bob_full}" id="in-call-dm-1">
                <in-call xmlns="urn:waddle:in-call:0" sid="dm-call-1">
                    <reaction emoji="👍"/>
                </in-call>
                <no-store xmlns="urn:xmpp:hints"/>
                <no-copy xmlns="urn:xmpp:hints"/>
            </message>"#
        ))
        .await
        .expect("send in-call reaction");

    let delivered = bob
        .recv_matching(|frame| frame.contains("urn:waddle:in-call:0") && frame.contains("👍"))
        .await
        .expect("bob receives in-call reaction");
    assert!(delivered.contains("dm-call-1"));
    assert!(delivered.contains("no-store"));
    assert!(delivered.contains("no-copy"));

    for (client, archive_jid, query_id) in [
        (&mut alice, alice_bare.as_str(), "mam-in-call-alice"),
        (&mut bob, bob_bare.as_str(), "mam-in-call-bob"),
    ] {
        client
            .send(&format!(
                r#"<iq type="set" id="{query_id}" to="{archive_jid}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
            ))
            .await
            .expect("send personal MAM query");
        let frames = client
            .recv_until(|frame| frame.contains(query_id) && frame.contains("<fin"))
            .await
            .expect("personal MAM frames");
        assert!(
            frames
                .iter()
                .all(|frame| !frame.contains("urn:waddle:in-call:0")),
            "transient in-call reaction must not be archived: {frames:?}"
        );
    }

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn muc_in_call_reaction_fans_out_to_room_and_is_not_archived() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[(BOB, BOB_PASSWORD)]);
    let alice_password = server.fixed_account_password().to_string();
    let mut alice = connect(&server, ALICE, &alice_password, "in-call-muc-alice").await;
    let mut bob = connect(&server, BOB, BOB_PASSWORD, "in-call-muc-bob").await;
    let room = format!("in-call-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut alice, &room, ALICE).await;
    join_room(&mut bob, &room, BOB).await;

    alice
        .send(&format!(
            r#"<message type="groupchat" to="{room}" id="in-call-muc-1">
                <in-call xmlns="urn:waddle:in-call:0" sid="muc-call-1">
                    <reaction emoji="🔥"/>
                </in-call>
                <no-store xmlns="urn:xmpp:hints"/>
                <no-copy xmlns="urn:xmpp:hints"/>
            </message>"#
        ))
        .await
        .expect("send room in-call reaction");

    let delivered = bob
        .recv_matching(|frame| frame.contains("urn:waddle:in-call:0") && frame.contains("🔥"))
        .await
        .expect("bob receives room in-call reaction");
    assert!(delivered.contains("muc-call-1"));

    alice
        .recv_matching(|frame| frame.contains("urn:waddle:in-call:0") && frame.contains("🔥"))
        .await
        .expect("alice receives own room echo");

    alice
        .send(&format!(
            r#"<iq type="set" id="mam-in-call-room" to="{room}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send room MAM query");
    let frames = alice
        .recv_until(|frame| frame.contains("mam-in-call-room") && frame.contains("<fin"))
        .await
        .expect("room MAM frames");
    assert!(
        frames
            .iter()
            .all(|frame| !frame.contains("urn:waddle:in-call:0")),
        "transient room in-call reaction must not be archived: {frames:?}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}
