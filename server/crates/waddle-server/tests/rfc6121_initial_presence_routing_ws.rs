//! RFC 6121 §4.2 / §8.5.2.1.1 over the WebSocket C2S transport: a
//! resource that has bound but not yet sent initial presence is a
//! "connected" resource, not an "available" one. A bare-JID chat message
//! goes to the account's available resources, so the silent resource
//! receives nothing while a sibling is available, and joins the delivery
//! set as soon as it sends initial presence.
//!
//! This is the server half of the Android client sending its own presence
//! at the head of every ready stream.

use std::time::Duration;

use jid::BareJid;
use minidom::Element;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use xmpp_parsers::{
    iq::Iq,
    message::{Id, Lang, Message, MessageType},
    ping::Ping,
    presence::{Presence, Type as PresenceType},
};

const DOMAIN: &str = "localhost";
const RECIPIENT_USER: &str = "admin";
const SENDER_USER: &str = "peer";
const SENDER_PASSWORD: &str = "peer-password-initial-presence";

/// Long enough for a misrouted copy to arrive once the sibling already has
/// its own: both copies leave the same routing pass.
const QUIET: Duration = Duration::from_millis(900);

async fn send(client: &mut WsXmppClient, element: Element) {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize XML");
    client
        .send(&String::from_utf8(bytes).expect("XML UTF-8"))
        .await
        .expect("send XML");
}

/// RFC 6121 §4.2.1 initial presence: no `to`, no `type`.
async fn send_initial_presence(client: &mut WsXmppClient) {
    send(client, Presence::new(PresenceType::None).into()).await;
}

/// XEP-0199 round trip: the server handles a stream's stanzas in order, so
/// the pong proves everything sent before it has been processed.
async fn barrier(client: &mut WsXmppClient, id: &str) {
    let mut ping = Iq::from_get(id, Ping);
    *ping.to_mut() = Some(DOMAIN.parse().expect("server JID"));
    send(client, ping.into()).await;
    client
        .recv_matching(|frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| element.name() == "iq" && element.attr("id") == Some(id))
        })
        .await
        .expect("barrier pong");
}

async fn send_bare_jid_dm(sender: &mut WsXmppClient, id: &str) {
    let to: BareJid = format!("{RECIPIENT_USER}@{DOMAIN}")
        .parse()
        .expect("recipient bare JID");
    let mut message = Message::new(Some(to.into()));
    message.id = Some(Id(id.to_owned()));
    message.type_ = MessageType::Chat;
    message.bodies.insert(Lang::new(), id.to_owned());
    send(sender, message.into()).await;
}

fn is_message_with_id(frame: &str, id: &str) -> bool {
    frame
        .parse::<Element>()
        .is_ok_and(|element| element.name() == "message" && element.attr("id") == Some(id))
}

async fn receive_dm(client: &mut WsXmppClient, id: &str) {
    client
        .recv_matching(|frame| is_message_with_id(frame, id))
        .await
        .unwrap_or_else(|error| panic!("expected DM {id}: {error}"));
}

/// Fails on any copy of message `id` until the stream stays quiet.
async fn assert_no_dm(client: &mut WsXmppClient, id: &str, why: &str) {
    while let Ok(frame) = client.recv_timeout(QUIET).await {
        assert!(!is_message_with_id(&frame, id), "{why}: {frame}");
    }
}

async fn recipient(server: &TestServer, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        RECIPIENT_USER,
        server.fixed_account_password(),
        &format!("{resource}-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("recipient connection")
}

#[tokio::test]
async fn bound_resource_joins_bare_jid_delivery_only_after_initial_presence() {
    let server = TestServer::start_with_extra_accounts(&[(SENDER_USER, SENDER_PASSWORD)]);

    let mut web = recipient(&server, "web").await;
    send_initial_presence(&mut web).await;
    barrier(&mut web, "web-available").await;

    // Bound, never announced: the Android client before this fix.
    let mut phone = recipient(&server, "phone").await;
    barrier(&mut phone, "phone-bound").await;

    let mut sender = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        SENDER_USER,
        SENDER_PASSWORD,
        &format!("sender-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("sender connection");

    send_bare_jid_dm(&mut sender, "dm-before-presence").await;
    receive_dm(&mut web, "dm-before-presence").await;
    assert_no_dm(
        &mut phone,
        "dm-before-presence",
        "a resource without presence must not share bare-JID delivery with an available sibling",
    )
    .await;

    send_initial_presence(&mut phone).await;
    barrier(&mut phone, "phone-available").await;

    send_bare_jid_dm(&mut sender, "dm-after-presence").await;
    receive_dm(&mut web, "dm-after-presence").await;
    receive_dm(&mut phone, "dm-after-presence").await;
    // Delivered live to the sibling, never stored offline: initial
    // presence has nothing to flush.
    assert_no_dm(
        &mut phone,
        "dm-before-presence",
        "initial presence must not replay a message another resource already received",
    )
    .await;

    let _ = sender.close().await;
    let _ = web.close().await;
    let _ = phone.close().await;
}
