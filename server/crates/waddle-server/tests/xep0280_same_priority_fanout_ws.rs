//! #1106 — bare-JID DM fan-out to same-priority resources must not
//! duplicate delivery or recipient-side persistence.
//!
//! RFC 6121 §8.5.2.1.1: a bare-JID chat message is delivered to every
//! available resource tied at the highest non-negative priority.
//! XEP-0280 §6.3: "The receiving server MUST NOT send a forwarded copy
//! to the client(s) the original <message/> stanza was addressed to, as
//! these recipients receive the original <message/> stanza." — the
//! whole RFC 6121 delivery set is the carbon exclusion set, not just
//! the single resource whose recipient pass emitted the carbon event.
//! XEP-0313 / XEP-0430: recipient-side archive and inbox writes are
//! keyed by the bare recipient and must happen once per message, not
//! once per delivered resource.

use waddle_ws_test_support as ws_common;

use std::time::Duration;
use ws_common::{TestServer, WsXmppClient};

const DOMAIN: &str = "localhost";
const RECIPIENT_USER: &str = "admin";
const SENDER_USER: &str = "peer";
const SENDER_PASSWORD: &str = "peer-password-1";
const NS_CARBONS: &str = "urn:xmpp:carbons:2";

async fn enable_carbons(client: &mut WsXmppClient, id: &str) {
    client
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="{id}"><enable xmlns="urn:xmpp:carbons:2"/></iq>"#
        ))
        .await
        .expect("send carbons enable");
    let _ = client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("carbons enable result");
}

async fn send_available_presence(client: &mut WsXmppClient) {
    client
        .send(r#"<presence xmlns="jabber:client"/>"#)
        .await
        .expect("send presence");
}

/// Drain every frame until the stream stays quiet, returning the ones
/// containing `needle`.
async fn drain_matching(client: &mut WsXmppClient, needle: &str) -> Vec<String> {
    let mut matching = Vec::new();
    while let Ok(frame) = client.recv_timeout(Duration::from_millis(900)).await {
        if frame.contains(needle) {
            matching.push(frame);
        }
    }
    matching
}

struct FanoutFixture {
    _server: TestServer,
    web: WsXmppClient,
    phone: WsXmppClient,
    sender: WsXmppClient,
}

/// Two recipient resources at identical (default 0) priority, both
/// carbons-enabled and presence-available, plus a distinct sender
/// account.
async fn fanout_fixture() -> FanoutFixture {
    let server = TestServer::start_with_extra_accounts(&[(SENDER_USER, SENDER_PASSWORD)]);
    let password = server.fixed_account_password().to_string();

    let mut web = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        RECIPIENT_USER,
        &password,
        &format!("web-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("web connection");
    enable_carbons(&mut web, "carbons-enable-web").await;
    send_available_presence(&mut web).await;

    let mut phone = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        RECIPIENT_USER,
        &password,
        &format!("phone-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("phone connection");
    enable_carbons(&mut phone, "carbons-enable-phone").await;
    send_available_presence(&mut phone).await;

    let sender = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        SENDER_USER,
        SENDER_PASSWORD,
        &format!("sender-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("sender connection");

    FanoutFixture {
        _server: server,
        web,
        phone,
        sender,
    }
}

#[tokio::test]
async fn two_same_priority_resources_receive_the_dm_exactly_once_each() {
    let mut fx = fanout_fixture().await;
    let body = format!("fanout-dedupe-proof-{}", uuid::Uuid::new_v4());

    fx.sender
        .send(&format!(
            r#"<message xmlns="jabber:client" to="admin@{DOMAIN}" type="chat" id="fanout-1"><body>{body}</body></message>"#
        ))
        .await
        .expect("send dm");

    let web_copies = drain_matching(&mut fx.web, &body).await;
    let phone_copies = drain_matching(&mut fx.phone, &body).await;

    for (name, copies) in [("web", &web_copies), ("phone", &phone_copies)] {
        assert_eq!(
            copies.len(),
            1,
            "{name} must receive the DM exactly once (XEP-0280 §6.3), got {}: {copies:?}",
            copies.len()
        );
        assert!(
            !copies[0].contains(NS_CARBONS),
            "{name}'s only copy must be the original delivery, not a carbon: {}",
            copies[0]
        );
    }

    let _ = fx.sender.close().await;
    let _ = fx.web.close().await;
    let _ = fx.phone.close().await;
}

#[tokio::test]
async fn recipient_archive_and_stanza_id_are_single_per_bare_recipient() {
    let mut fx = fanout_fixture().await;
    let body = format!("fanout-archive-proof-{}", uuid::Uuid::new_v4());

    fx.sender
        .send(&format!(
            r#"<message xmlns="jabber:client" to="admin@{DOMAIN}" type="chat" id="fanout-2"><body>{body}</body></message>"#
        ))
        .await
        .expect("send dm");

    let web_copies = drain_matching(&mut fx.web, &body).await;
    let phone_copies = drain_matching(&mut fx.phone, &body).await;
    assert!(
        !web_copies.is_empty() && !phone_copies.is_empty(),
        "both resources must receive the DM (web: {web_copies:?}, phone: {phone_copies:?})"
    );
    // XEP-0359: both resources must see the SAME recipient-stamped
    // stanza-id — one recipient pass, one stamp. Divergent ids break
    // client-side live/MAM dedup.
    let web_id = stanza_id_of(&web_copies[0]);
    let phone_id = stanza_id_of(&phone_copies[0]);
    assert_eq!(
        web_id, phone_id,
        "both resources must carry the same recipient <stanza-id/> \
         (web: {web_id:?} / phone: {phone_id:?})"
    );

    // XEP-0313: exactly one archived row for the message.
    fx.web
        .send(&format!(
            r#"<iq xmlns="jabber:client" type="set" id="fanout-mam-1" to="admin@{DOMAIN}"><query xmlns="urn:xmpp:mam:2"/></iq>"#
        ))
        .await
        .expect("send mam query");
    let mut archived = 0usize;
    loop {
        let frame = fx
            .web
            .recv_timeout(Duration::from_secs(3))
            .await
            .expect("mam stream frame");
        if frame.contains(&body) && frame.contains("urn:xmpp:mam:2") {
            archived += 1;
        }
        if frame.contains("<fin") {
            break;
        }
    }
    assert_eq!(
        archived, 1,
        "recipient archive must hold exactly one row for the message"
    );

    let _ = fx.sender.close().await;
    let _ = fx.web.close().await;
    let _ = fx.phone.close().await;
}

#[tokio::test]
async fn recipient_inbox_unread_increments_once_per_message() {
    let mut fx = fanout_fixture().await;
    let body = format!("fanout-inbox-proof-{}", uuid::Uuid::new_v4());

    fx.sender
        .send(&format!(
            r#"<message xmlns="jabber:client" to="admin@{DOMAIN}" type="chat" id="fanout-3"><body>{body}</body></message>"#
        ))
        .await
        .expect("send dm");

    // Wait until the message reached both resources so persistence has
    // settled before querying the inbox.
    let web_copies = drain_matching(&mut fx.web, &body).await;
    let phone_copies = drain_matching(&mut fx.phone, &body).await;
    assert!(
        !web_copies.is_empty() && !phone_copies.is_empty(),
        "both resources must receive the DM before the inbox assert"
    );

    fx.web
        .send(
            r#"<iq xmlns="jabber:client" type="get" id="fanout-inbox-1"><inbox xmlns="urn:xmpp:inbox:1" messages="false"/></iq>"#,
        )
        .await
        .expect("send inbox query");
    let mut unread: Option<String> = None;
    loop {
        let frame = fx
            .web
            .recv_timeout(Duration::from_secs(3))
            .await
            .expect("inbox stream frame");
        if frame.contains("<entry") && frame.contains(&format!("peer@{DOMAIN}")) {
            unread = attr_value(&frame, "unread");
        }
        if frame.contains("<fin") {
            break;
        }
    }
    assert_eq!(
        unread.as_deref(),
        Some("1"),
        "one DM must increment the recipient inbox unread exactly once"
    );

    let _ = fx.sender.close().await;
    let _ = fx.web.close().await;
    let _ = fx.phone.close().await;
}

/// Extract the `id` of the recipient-stamped XEP-0359 `<stanza-id/>`.
fn stanza_id_of(frame: &str) -> Option<String> {
    let marker = "urn:xmpp:sid:0";
    let start = frame.find(marker)?;
    let scope = &frame[start.saturating_sub(200)..(start + 200).min(frame.len())];
    attr_value(scope, "id")
}

fn attr_value(frame: &str, attr: &str) -> Option<String> {
    let double = format!("{attr}=\"");
    if let Some(start) = frame.find(&double).map(|start| start + double.len()) {
        let end = frame[start..].find('"')?;
        return Some(frame[start..start + end].to_string());
    }
    let single = format!("{attr}='");
    let start = frame.find(&single).map(|start| start + single.len())?;
    let end = frame[start..].find('\'')?;
    Some(frame[start..start + end].to_string())
}
