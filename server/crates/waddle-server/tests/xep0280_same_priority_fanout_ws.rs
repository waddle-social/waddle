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
use waddle_xmpp::ns::{JABBER_CLIENT, SM as SM_NS};
use waddle_xmpp::xep::xep0430::NS_INBOX;
use waddle_xmpp_core::carbons::CARBONS_NS;
use waddle_xmpp_core::mam::MAM_NS;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const RECIPIENT_USER: &str = "admin";
const SENDER_USER: &str = "peer";
const SENDER_PASSWORD: &str = "peer-password-1";

fn element_to_xml(element: Element) -> String {
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize XML");
    String::from_utf8(buf).expect("UTF-8")
}

fn attr(name: &str) -> minidom::rxml::NcName {
    minidom::rxml::NcNameStr::from_str(name)
        .expect("static attribute name is a valid NcName")
        .to_owned()
}

fn iq_xml(kind: &str, id: &str, to: Option<&str>, payload: Element) -> String {
    let mut builder = Element::builder("iq", JABBER_CLIENT)
        .attr(attr("type"), kind)
        .attr(attr("id"), id);
    if let Some(to) = to {
        builder = builder.attr(attr("to"), to);
    }
    element_to_xml(builder.append(payload).build())
}

fn carbons_enable_xml(id: &str) -> String {
    iq_xml(
        "set",
        id,
        None,
        Element::builder("enable", CARBONS_NS).build(),
    )
}

fn chat_message_xml(to: &str, id: &str, body: &str) -> String {
    let mut body_el = Element::builder("body", JABBER_CLIENT).build();
    body_el.append_text_node(body);
    element_to_xml(
        Element::builder("message", JABBER_CLIENT)
            .attr(attr("to"), to)
            .attr(attr("type"), "chat")
            .attr(attr("id"), id)
            .append(body_el)
            .build(),
    )
}

fn sm_enable_xml() -> String {
    element_to_xml(
        Element::builder("enable", SM_NS)
            .attr(attr("resume"), "true")
            .build(),
    )
}

fn sm_resume_xml(previd: &str) -> String {
    element_to_xml(
        Element::builder("resume", SM_NS)
            .attr(attr("previd"), previd)
            .attr(attr("h"), "0")
            .build(),
    )
}

fn mam_query_xml(id: &str, archive_jid: &str) -> String {
    iq_xml(
        "set",
        id,
        Some(archive_jid),
        Element::builder("query", MAM_NS).build(),
    )
}

fn inbox_query_xml(id: &str) -> String {
    iq_xml(
        "get",
        id,
        None,
        Element::builder("inbox", NS_INBOX)
            .attr(attr("messages"), "false")
            .build(),
    )
}

async fn enable_carbons(client: &mut WsXmppClient, id: &str) {
    client
        .send(&carbons_enable_xml(id))
        .await
        .expect("send carbons enable");
    let _ = client
        .recv_matching(|frame| frame.contains(id))
        .await
        .expect("carbons enable result");
}

async fn send_available_presence(client: &mut WsXmppClient) {
    client
        .send(&element_to_xml(
            Element::builder("presence", JABBER_CLIENT).build(),
        ))
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
        .send(&chat_message_xml(
            &format!("admin@{DOMAIN}"),
            "fanout-1",
            &body,
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
            !copies[0].contains(CARBONS_NS),
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
        .send(&chat_message_xml(
            &format!("admin@{DOMAIN}"),
            "fanout-2",
            &body,
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
        .send(&mam_query_xml("fanout-mam-1", &format!("admin@{DOMAIN}")))
        .await
        .expect("send mam query");
    let mut archived = 0usize;
    loop {
        let frame = fx
            .web
            .recv_timeout(Duration::from_secs(3))
            .await
            .expect("mam stream frame");
        if frame.contains(&body) && frame.contains(MAM_NS) {
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
        .send(&chat_message_xml(
            &format!("admin@{DOMAIN}"),
            "fanout-3",
            &body,
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
        .send(&inbox_query_xml("fanout-inbox-1"))
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

#[tokio::test]
async fn detached_carbons_enabled_sibling_gets_the_dm_exactly_once_on_resume() {
    // A detached-but-resumable resource is also a "client the original
    // <message/> stanza was addressed to" (its XEP-0198 buffer gets the
    // original queued for replay) — XEP-0280 §6.3 forbids ALSO queueing
    // a received-carbon for it.
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
        &format!("phone-detached-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("phone connection");
    enable_carbons(&mut phone, "carbons-enable-phone").await;
    phone
        .send(&sm_enable_xml())
        .await
        .expect("enable resumption");
    let enabled = phone
        .recv_matching(|frame| frame.contains("<enabled"))
        .await
        .expect("sm enabled");
    let stream_id = attr_value(&enabled, "id").expect("enabled missing id");
    drop(phone);

    let mut sender = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        SENDER_USER,
        SENDER_PASSWORD,
        &format!("sender-{}", uuid::Uuid::new_v4()),
    )
    .await
    .expect("sender connection");
    let body = format!("detached-dedupe-proof-{}", uuid::Uuid::new_v4());
    sender
        .send(&chat_message_xml(
            &format!("admin@{DOMAIN}"),
            "fanout-4",
            &body,
        ))
        .await
        .expect("send dm");
    // Wait for the live resource's copy so queueing for the detached
    // sibling has completed before we resume.
    let web_copies = drain_matching(&mut web, &body).await;
    assert!(!web_copies.is_empty(), "web must receive the DM live");

    let mut resumed = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("resume connection");
    resumed
        .authenticate(DOMAIN, RECIPIENT_USER, &password)
        .await
        .expect("authenticate resume connection");
    resumed
        .send(&sm_resume_xml(&stream_id))
        .await
        .expect("send resume");
    let _ = resumed
        .recv_matching(|frame| frame.contains("<resumed"))
        .await
        .expect("sm resumed");

    let replayed = drain_matching(&mut resumed, &body).await;
    assert_eq!(
        replayed.len(),
        1,
        "detached sibling must get the DM exactly once on resume \
         (original replay only, no received-carbon — XEP-0280 §6.3), got: {replayed:?}"
    );
    assert!(
        !replayed[0].contains(CARBONS_NS),
        "the replayed copy must be the original delivery, not a carbon: {}",
        replayed[0]
    );

    let _ = sender.close().await;
    let _ = web.close().await;
    let _ = resumed.close().await;
}

/// Extract the `id` of the XEP-0359 `<stanza-id/>` stamped by the
/// recipient's bare JID (`by='admin@DOMAIN'`). Walks each `<stanza-id`
/// element so the message's own `id` attribute (identical across
/// resources by construction) can never satisfy the assertion.
fn stanza_id_of(frame: &str) -> Option<String> {
    let recipient_bare = format!("admin@{DOMAIN}");
    let mut rest = frame;
    while let Some(start) = rest.find("<stanza-id") {
        let element_rest = &rest[start..];
        let end = element_rest.find("/>")?;
        let element = &element_rest[..end + 2];
        if attr_value(element, "by").as_deref() == Some(recipient_bare.as_str()) {
            return attr_value(element, "id");
        }
        rest = &element_rest[end + 2..];
    }
    None
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
