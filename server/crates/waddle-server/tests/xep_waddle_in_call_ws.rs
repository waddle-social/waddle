//! Waddle in-call signaling integration tests over WebSocket.

mod ws_common;

use minidom::Element;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const ALICE: &str = "admin";
const BOB: &str = "bob";
const BOB_PASSWORD: &str = "bob-password";
const CAROL: &str = "carol";
const CAROL_PASSWORD: &str = "carol-password";
const NS_CLIENT: &str = "jabber:client";
const NS_HINTS: &str = "urn:xmpp:hints";
const NS_MAM: &str = "urn:xmpp:mam:2";
const NS_MUC: &str = "http://jabber.org/protocol/muc";
const NS_MUJI: &str = "urn:xmpp:jingle:muji:0";
const NS_RTP: &str = "urn:xmpp:jingle:apps:rtp:1";
const NS_WADDLE_IN_CALL: &str = "urn:waddle:in-call:0";
static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

fn attr(name: &'static str) -> minidom::rxml::NcName {
    minidom::rxml::NcName::try_from(name).expect("valid XML attribute name")
}

fn serialize(element: Element) -> String {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize XML");
    String::from_utf8(bytes).expect("XML is UTF-8")
}

fn muc_join_presence(room: &str, nick: &str) -> String {
    serialize(
        Element::builder("presence", NS_CLIENT)
            .attr(attr("to"), format!("{room}/{nick}"))
            .append(Element::builder("x", NS_MUC).build())
            .build(),
    )
}

fn in_call_reaction_message(
    to: &str,
    message_type: &str,
    id: &str,
    sid: &str,
    emoji: &str,
) -> String {
    serialize(
        Element::builder("message", NS_CLIENT)
            .attr(attr("type"), message_type)
            .attr(attr("to"), to)
            .attr(attr("id"), id)
            .append(
                Element::builder("in-call", NS_WADDLE_IN_CALL)
                    .attr(attr("sid"), sid)
                    .append(
                        Element::builder("reaction", NS_WADDLE_IN_CALL)
                            .attr(attr("emoji"), emoji)
                            .build(),
                    )
                    .build(),
            )
            .append(Element::builder("no-store", NS_HINTS).build())
            .append(Element::builder("no-copy", NS_HINTS).build())
            .build(),
    )
}

/// A MUC call presence: an active XEP-0272 `<muji/>` advertisement
/// (one audio content), optionally carrying the `urn:waddle:in-call:0`
/// `<hand-raised/>` presence state *alongside* (never inside) `<muji/>`.
fn call_presence(room: &str, nick: &str, hand_raised: bool) -> String {
    let muji = Element::builder("muji", NS_MUJI)
        .append(
            Element::builder("content", NS_MUJI)
                .attr(attr("creator"), "initiator")
                .attr(attr("name"), "audio")
                .append(
                    Element::builder("description", NS_RTP)
                        .attr(attr("media"), "audio")
                        .build(),
                )
                .build(),
        )
        .build();
    let mut presence = Element::builder("presence", NS_CLIENT)
        .attr(attr("to"), format!("{room}/{nick}"))
        .append(Element::builder("x", NS_MUC).build())
        .append(muji);
    if hand_raised {
        presence = presence.append(
            Element::builder("in-call", NS_WADDLE_IN_CALL)
                .append(Element::builder("hand-raised", NS_WADDLE_IN_CALL).build())
                .build(),
        );
    }
    serialize(presence.build())
}

fn presence_in_call_child(frame: &str) -> Option<Element> {
    let element: Element = frame.parse().ok()?;
    if element.name() != "presence" {
        return None;
    }
    element
        .children()
        .find(|c| c.name() == "in-call" && c.ns() == NS_WADDLE_IN_CALL)
        .cloned()
}

fn mam_query(to: &str, id: &str) -> String {
    serialize(
        Element::builder("iq", NS_CLIENT)
            .attr(attr("type"), "set")
            .attr(attr("id"), id)
            .attr(attr("to"), to)
            .append(Element::builder("query", NS_MAM).build())
            .build(),
    )
}

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
        .send(&muc_join_presence(room, nick))
        .await
        .expect("send join");
    client
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("join responses");
}

#[tokio::test]
async fn raise_hand_presence_reflects_in_call_child_alongside_muji() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[(BOB, BOB_PASSWORD)]);
    let alice_password = server.fixed_account_password().to_string();
    let mut alice = connect(&server, ALICE, &alice_password, "hand-alice").await;
    let mut bob = connect(&server, BOB, BOB_PASSWORD, "hand-bob").await;
    let room = format!("hand-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut alice, &room, ALICE).await;
    join_room(&mut bob, &room, BOB).await;

    // Alice enters the call and raises her hand in one presence update.
    alice
        .send(&call_presence(&room, ALICE, true))
        .await
        .expect("alice raises hand");

    // Bob receives Alice's reflected presence carrying BOTH the muji call
    // advertisement and the raised-hand `<in-call>` child, with the
    // `<in-call>` sibling of `<muji>` rather than nested inside it.
    let frame = bob
        .recv_matching(|f| f.contains("<presence") && f.contains("hand-raised"))
        .await
        .expect("bob sees alice's raised hand");
    let element: Element = frame.parse().expect("presence xml");
    assert_eq!(
        element.attr("from"),
        Some(format!("{room}/{ALICE}").as_str()),
        "reflection is from room/nick: {frame}"
    );
    let in_call = element
        .children()
        .find(|c| c.name() == "in-call" && c.ns() == NS_WADDLE_IN_CALL)
        .unwrap_or_else(|| panic!("reflection carries an <in-call> child: {frame}"));
    assert!(
        in_call
            .children()
            .any(|c| c.name() == "hand-raised" && c.ns() == NS_WADDLE_IN_CALL),
        "<in-call> carries the <hand-raised/> marker: {frame}"
    );
    let muji = element
        .children()
        .find(|c| c.name() == "muji" && c.ns() == NS_MUJI)
        .unwrap_or_else(|| panic!("reflection still carries <muji>: {frame}"));
    assert!(
        muji.children().all(|c| c.name() != "in-call"),
        "<in-call> must sit alongside, never inside, <muji>: {frame}"
    );

    let _ = bob.close().await;
    let _ = alice.close().await;
}

#[tokio::test]
async fn raised_hand_replays_to_late_joiner_and_clears_on_lower() {
    let _guard = TEST_SERIAL.lock().await;
    let server =
        TestServer::start_with_extra_accounts(&[(BOB, BOB_PASSWORD), (CAROL, CAROL_PASSWORD)]);
    let alice_password = server.fixed_account_password().to_string();
    let mut alice = connect(&server, ALICE, &alice_password, "late-alice").await;
    let room = format!("hand-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    join_room(&mut alice, &room, ALICE).await;

    // Alice enters the call and raises her hand, then drains her own echo
    // so the room actor has committed the state before Carol arrives.
    alice
        .send(&call_presence(&room, ALICE, true))
        .await
        .expect("alice raises hand");
    alice
        .recv_matching(|f| f.contains("hand-raised"))
        .await
        .expect("alice self echo");

    // Carol joins mid-call: her XEP-0045 occupant-list replay must already
    // show Alice's raised hand — no fresh update is coming for a steady call.
    let mut carol = connect(&server, CAROL, CAROL_PASSWORD, "late-carol").await;
    carol
        .send(&muc_join_presence(&room, CAROL))
        .await
        .expect("carol joins");
    let replay = carol
        .recv_until(|f| f.contains("<subject"))
        .await
        .expect("carol join replay");
    let alice_from = format!("{room}/{ALICE}");
    assert!(
        replay
            .iter()
            .any(|f| f.contains(&alice_from) && f.contains("hand-raised")),
        "carol's join replay shows alice's raised hand: {replay:?}"
    );

    // Alice lowers her hand (stays in the call): the reflected presence drops
    // the in-call child for everyone.
    alice
        .send(&call_presence(&room, ALICE, false))
        .await
        .expect("alice lowers hand");
    let lowered = carol
        .recv_matching(|f| f.contains("<presence") && f.contains(&alice_from) && f.contains("muji"))
        .await
        .expect("carol sees alice's lowered presence");
    assert!(
        presence_in_call_child(&lowered)
            .map(|c| c.children().all(|g| g.name() != "hand-raised"))
            .unwrap_or(true),
        "lowered presence carries no raised-hand marker: {lowered}"
    );

    let _ = carol.close().await;
    let _ = alice.close().await;
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
        .send(&in_call_reaction_message(
            &bob_full,
            "chat",
            "in-call-dm-1",
            "dm-call-1",
            "👍",
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
            .send(&mam_query(archive_jid, query_id))
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
        .send(&in_call_reaction_message(
            &room,
            "groupchat",
            "in-call-muc-1",
            "muc-call-1",
            "🔥",
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
        .send(&mam_query(&room, "mam-in-call-room"))
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
