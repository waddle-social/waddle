//! XEP-0334 §4: <store/> enables archival without changing headline routing.

pub mod pending_delivery_ws_support;

use jid::Jid;
use pending_delivery_ws_support::{
    barrier_without_message, enable, presence, recv, send, PendingFixture,
};
use std::time::Duration;
use waddle_ws_test_support::WsXmppClient;
use waddle_xmpp::xep::xep0334::{add_hint, has_hint, Hint};
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

async fn send_stored_headline(sender: &mut WsXmppClient, to: Jid, id: &str) {
    let mut message = Message::new(Some(to));
    message.type_ = MessageType::Headline;
    message.id = Some(Id(id.to_owned()));
    message.bodies.insert(Lang::new(), id.to_owned());
    add_hint(&mut message, Hint::Store);
    send(sender, message.into()).await;
}

async fn expect_headline(client: &mut WsXmppClient, id: &str) -> Message {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let frame = recv(client).await;
            if !frame.is("message", xmpp_parsers::ns::JABBER_CLIENT) {
                continue;
            }
            let message = Message::try_from(frame).expect("typed routed message");
            // Ignore unrelated server notifications, but never discard a
            // tested headline: an incorrectly routed earlier stanza fails.
            if message
                .id
                .as_ref()
                .is_some_and(|id| id.0.starts_with("hint-headline-"))
            {
                assert_eq!(message.id.as_ref().map(|value| value.0.as_str()), Some(id));
                assert_eq!(message.type_, MessageType::Headline);
                assert!(has_hint(&message, Hint::Store));
                return message;
            }
        }
    })
    .await
    .expect("expected headline on the real WebSocket")
}

async fn archive_count(fixture: &PendingFixture) -> i64 {
    let db = fixture.storage.database();
    let guard = db.guard().await.expect("archive inspection database");
    let mut rows = guard
        .query(
            "SELECT COUNT(*) FROM mam_messages WHERE room_jid = ?",
            waddle_server::db_params![fixture.recipient.to_string()],
        )
        .await
        .expect("recipient archive count");
    rows.next()
        .await
        .expect("count row")
        .expect("archive count")
        .get(0)
        .expect("integer count")
}

async fn wait_for_terminal_messages(fixture: &PendingFixture, expected: i64) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            let db = fixture.storage.database();
            let guard = db.guard().await.expect("ingress inspection database");
            let mut rows = guard
                .query(
                    "SELECT COUNT(*) FROM ingress_messages WHERE terminal_at IS NOT NULL",
                    (),
                )
                .await
                .expect("settled ingress count");
            let count: i64 = rows
                .next()
                .await
                .expect("count row")
                .expect("count")
                .get(0)
                .expect("integer count");
            if count >= expected {
                return;
            }
            drop(rows);
            drop(guard);
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("message routing effects settle");
}

#[tokio::test]
async fn stored_headlines_keep_bare_fanout_and_exact_full_jid_routing() {
    let fixture = PendingFixture::start().await;
    let mut sender = fixture.connect("alice", "sender").await;
    let mut high = fixture.connect("bob", "high").await;
    let mut low = fixture.connect("bob", "low").await;
    let mut negative = fixture.connect("bob", "negative").await;
    for (client, priority) in [(&mut high, 10), (&mut low, 0), (&mut negative, -1)] {
        enable(client).await;
        presence(client, priority).await;
        barrier_without_message(client, &mut 0, 1).await;
    }
    assert_eq!(archive_count(&fixture).await, 0);

    send_stored_headline(
        &mut sender,
        fixture.recipient.clone().into(),
        "hint-headline-bare",
    )
    .await;
    let high_message = expect_headline(&mut high, "hint-headline-bare").await;
    let low_message = expect_headline(&mut low, "hint-headline-bare").await;
    let recipient_stamp = |message: &Message| {
        waddle_xmpp_core::xep0359::extract_stanza_ids(message)
            .into_iter()
            .find(|stamp| stamp.by == fixture.recipient)
            .expect("recipient archive identity on delivered headline")
    };
    assert_eq!(
        recipient_stamp(&high_message),
        recipient_stamp(&low_message)
    );
    assert_eq!(
        archive_count(&fixture).await,
        1,
        "one frozen recipient archive pass feeds both priorities"
    );

    // Negative priority excludes a bare-JID headline, but does not prevent
    // exact full-JID delivery. Receiving this later frame is a positive
    // witness that the earlier bare headline did not reach this resource.
    send_stored_headline(
        &mut sender,
        "bob@localhost/negative".parse().expect("negative resource"),
        "hint-headline-negative-full",
    )
    .await;
    expect_headline(&mut negative, "hint-headline-negative-full").await;
    assert_eq!(archive_count(&fixture).await, 2);

    // A missing full resource must neither fall back to chat-style highest
    // priority delivery nor manufacture offline pending custody.
    send_stored_headline(
        &mut sender,
        "bob@localhost/missing".parse().expect("missing resource"),
        "hint-headline-missing-full",
    )
    .await;
    wait_for_terminal_messages(&fixture, 3).await;
    assert!(
        fixture.rows().await.is_empty(),
        "unmatched full headlines are not offline chat messages"
    );
    assert_eq!(
        archive_count(&fixture).await,
        2,
        "unmatched full headline has no recipient archive pass"
    );
    for (client, resource, id) in [
        (
            &mut high,
            "bob@localhost/high",
            "hint-headline-high-witness",
        ),
        (&mut low, "bob@localhost/low", "hint-headline-low-witness"),
        (
            &mut negative,
            "bob@localhost/negative",
            "hint-headline-negative-witness",
        ),
    ] {
        send_stored_headline(&mut sender, resource.parse().expect("witness resource"), id).await;
        expect_headline(client, id).await;
    }
}
