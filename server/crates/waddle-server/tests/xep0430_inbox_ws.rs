//! XEP-0430 Inbox conformance tests over the WebSocket transport.
//!
//! These tests pin the standards-track wire shape Waddle now serves
//! under `urn:xmpp:inbox:1`:
//!
//! 1. Empty inbox: a single `<iq type='result'><fin total='0'/></iq>`
//!    with no streamed entry messages.
//! 2. Populated inbox: one `<message><entry/></message>` per
//!    conversation followed by the `<fin/>` IQ; `total` matches the
//!    streamed entry count.
//! 3. `unread-only='true'` filters out conversations with unread=0.
//! 4. `messages='false'` elides the embedded MAM `<result/>` payload
//!    and emits only the bare `<entry/>` element.
//! 5. RSM `<max/>` pages the response and the `<fin/>` carries an RSM
//!    `<set/>` with `first`/`last`/`count`.
//! 6. A XEP-0333 displayed marker decrements unread state, reflected
//!    in the next inbox query.

use waddle_ws_test_support as ws_common;

use std::str::FromStr;
use tokio::sync::Mutex;
use ws_common::{TestServer, WsXmppClient};
use xmpp_parsers::minidom::Element;

const DOMAIN: &str = "localhost";
const PRIMARY_USER: &str = "admin";
const PEER_USER: &str = "peer";
const PEER_PASSWORD: &str = "peer-password-1";
const CLIENT_NS: &str = "jabber:client";
const NS_INBOX: &str = "urn:xmpp:inbox:1";
const NS_MAM: &str = "urn:xmpp:mam:2";
const NS_FORWARD: &str = "urn:xmpp:forward:0";
const NS_RSM: &str = "http://jabber.org/protocol/rsm";
const NS_CHAT_MARKERS: &str = "urn:xmpp:chat-markers:0";

static TEST_SERIAL: Mutex<()> = Mutex::const_new(());

async fn primary_client(server: &TestServer, resource: &str) -> WsXmppClient {
    let password = server.fixed_account_password().to_string();
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, PRIMARY_USER, &password, resource)
        .await
        .expect("connect primary")
}

async fn peer_client(server: &TestServer, resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, PEER_USER, PEER_PASSWORD, resource)
        .await
        .expect("connect peer")
}

fn element_to_xml(element: Element) -> String {
    let mut buf = Vec::new();
    element.write_to(&mut buf).expect("serialize XML");
    String::from_utf8(buf).expect("UTF-8")
}

/// Build a XEP-0430 inbox query IQ. All knobs are explicit so the test
/// table reads as the request wire-shape it pins.
fn inbox_query_iq(
    id: &str,
    unread_only: Option<bool>,
    messages: Option<bool>,
    rsm_max: Option<u32>,
) -> String {
    let mut inbox = Element::builder("inbox", NS_INBOX).build();
    if let Some(value) = unread_only {
        inbox.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("unread-only").to_owned(),
            if value { "true" } else { "false" },
        );
    }
    if let Some(value) = messages {
        inbox.set_attr(
            minidom::rxml::Namespace::NONE,
            minidom::rxml::xml_ncname!("messages").to_owned(),
            if value { "true" } else { "false" },
        );
    }
    if let Some(max) = rsm_max {
        let mut set = Element::builder("set", NS_RSM).build();
        let mut max_el = Element::builder("max", NS_RSM).build();
        max_el.append_text_node(max.to_string());
        set.append_child(max_el);
        inbox.append_child(set);
    }
    let iq = Element::builder("iq", CLIENT_NS)
        .attr(minidom::rxml::xml_ncname!("type").to_owned(), "get")
        .attr(minidom::rxml::xml_ncname!("id").to_owned(), id)
        .append(inbox)
        .build();
    element_to_xml(iq)
}

/// Read every frame from the inbox query response. The stream
/// terminates with the `<iq type='result'>` containing `<fin/>`. We
/// filter frames by the IQ id, by `queryid='<id>'` on embedded MAM
/// results, or by the query response's direct official `<entry/>`.
/// Unsolicited push headlines use the private `<push/>` wrapper, so
/// direct non-headline entries are safe to keep here.
async fn collect_inbox_response(client: &mut WsXmppClient, id: &str) -> Vec<String> {
    let queryid_marker = format!("queryid='{id}'");
    let mut frames = Vec::new();
    loop {
        let frame = client
            .recv_matching(|candidate| {
                let matches_fin = candidate.contains("<iq") && candidate.contains(id);
                let matches_entry = (candidate.contains("<message")
                    && candidate.contains(queryid_marker.as_str()))
                    || is_inbox_query_entry_frame(candidate);
                matches_fin || matches_entry
            })
            .await
            .expect("inbox response frame");
        let is_fin_iq = frame.contains("<iq") && frame.contains(id) && frame.contains("<fin");
        frames.push(frame);
        if is_fin_iq {
            return frames;
        }
    }
}

fn is_inbox_query_entry_frame(candidate: &str) -> bool {
    let Ok(element) = Element::from_str(candidate) else {
        return false;
    };
    element.name() == "message"
        && element.attr("type") != Some("headline")
        && element
            .children()
            .any(|child| child.name() == "entry" && child.ns() == NS_INBOX)
}

fn parse_fin(xml: &str) -> Element {
    let element = Element::from_str(xml).expect("valid fin IQ XML");
    assert_eq!(element.name(), "iq");
    assert_eq!(
        element.attr("type"),
        Some("result"),
        "fin must be a result IQ: {xml}"
    );
    element
        .children()
        .find(|c| c.name() == "fin" && c.ns() == NS_INBOX)
        .cloned()
        .unwrap_or_else(|| panic!("no <fin xmlns='{NS_INBOX}'/> in {xml}"))
}

fn entries_from_response(frames: &[String]) -> Vec<Element> {
    frames
        .iter()
        .filter_map(|frame| Element::from_str(frame).ok())
        .filter(|el| el.name() == "message")
        .filter_map(|message| {
            message
                .children()
                .find(|c| c.name() == "entry" && c.ns() == NS_INBOX)
                .cloned()
        })
        .collect()
}

fn mam_results_from_response(frames: &[String]) -> Vec<Element> {
    frames
        .iter()
        .filter_map(|frame| Element::from_str(frame).ok())
        .filter(|el| el.name() == "message")
        .filter_map(|message| {
            message
                .children()
                .find(|c| c.name() == "result" && c.ns() == NS_MAM)
                .cloned()
        })
        .collect()
}

/// Send a DM and wait for the recipient's delivered frame so the
/// inbox projection has surely run.
async fn send_dm(
    sender: &mut WsXmppClient,
    recipient: &mut WsXmppClient,
    recipient_bare: &str,
    msg_id: &str,
    body: &str,
) {
    sender
        .send(&format!(
            r#"<message xmlns='{CLIENT_NS}' type='chat' to='{recipient_bare}' id='{msg_id}'><body>{body}</body></message>"#
        ))
        .await
        .expect("send dm");
    recipient
        .recv_matching(|frame| frame.contains(msg_id))
        .await
        .expect("dm delivered");
}

#[tokio::test]
async fn empty_inbox_returns_fin_only() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[(PEER_USER, PEER_PASSWORD)]);
    let mut client = primary_client(&server, "ib-empty").await;

    client
        .send(&inbox_query_iq("ib-empty-1", None, None, None))
        .await
        .expect("send inbox query");
    let frames = collect_inbox_response(&mut client, "ib-empty-1").await;
    assert_eq!(
        entries_from_response(&frames).len(),
        0,
        "empty inbox emits no streamed entry messages: {frames:?}"
    );
    let fin = parse_fin(frames.last().expect("at least the fin IQ"));
    assert_eq!(fin.attr("total"), Some("0"), "empty fin total: {fin:?}");
    assert_eq!(fin.attr("unread"), Some("0"), "empty fin unread: {fin:?}");

    let _ = client.close().await;
}

#[tokio::test]
async fn populated_inbox_streams_entry_per_conversation() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[(PEER_USER, PEER_PASSWORD)]);
    let mut primary = primary_client(&server, "ib-pop").await;
    let mut peer = peer_client(&server, "ib-pop-peer").await;

    let primary_bare = format!("{PRIMARY_USER}@{DOMAIN}");
    send_dm(&mut peer, &mut primary, &primary_bare, "msg-pop-1", "hi 1").await;
    send_dm(&mut peer, &mut primary, &primary_bare, "msg-pop-2", "hi 2").await;
    // Give the inbox projection a beat to settle.
    tokio::time::sleep(std::time::Duration::from_millis(150)).await;

    primary
        .send(&inbox_query_iq("ib-pop-1", None, None, None))
        .await
        .expect("send inbox query");
    let frames = collect_inbox_response(&mut primary, "ib-pop-1").await;
    let entries = entries_from_response(&frames);
    assert_eq!(
        entries.len(),
        1,
        "two DMs from the same peer collapse into one inbox row: {frames:?}"
    );
    let peer_bare = format!("{PEER_USER}@{DOMAIN}");
    assert_eq!(entries[0].attr("jid"), Some(peer_bare.as_str()));
    assert_eq!(
        entries[0].attr("unread"),
        Some("2"),
        "two unread messages: {entries:?}"
    );

    let fin = parse_fin(frames.last().expect("fin"));
    assert_eq!(fin.attr("total"), Some("1"));
    assert_eq!(fin.attr("unread"), Some("1"));

    // `messages='true'` default: the streamed message must carry a
    // MAM `<result/>` with the forwarded body of the latest DM.
    let results = mam_results_from_response(&frames);
    assert_eq!(results.len(), 1, "messages=true embeds one MAM result");
    let forwarded = results[0]
        .children()
        .find(|c| c.name() == "forwarded" && c.ns() == NS_FORWARD)
        .expect("forwarded element");
    let inner = forwarded
        .children()
        .find(|c| c.name() == "message")
        .expect("forwarded inner message");
    let body = inner
        .children()
        .find(|c| c.name() == "body")
        .map(|el| el.text())
        .unwrap_or_default();
    assert_eq!(
        body, "hi 2",
        "latest DM body in forwarded result: {inner:?}"
    );

    let _ = primary.close().await;
    let _ = peer.close().await;
}

#[tokio::test]
async fn unread_only_filters_zero_unread_rows() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[(PEER_USER, PEER_PASSWORD)]);
    let mut primary = primary_client(&server, "ib-uo").await;
    let mut peer = peer_client(&server, "ib-uo-peer").await;

    let primary_bare = format!("{PRIMARY_USER}@{DOMAIN}");
    let peer_bare = format!("{PEER_USER}@{DOMAIN}");
    send_dm(
        &mut peer,
        &mut primary,
        &primary_bare,
        "msg-uo-1",
        "hi unread",
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    // Mark-read clears unread for the peer DM.
    primary
        .send(&format!(
            r#"<iq xmlns='{CLIENT_NS}' type='set' to='{primary_bare}' id='ib-uo-mark'>
                <mark-read xmlns='urn:waddle:inbox:0' partner='{peer_bare}'/>
            </iq>"#
        ))
        .await
        .expect("send mark-read");
    primary
        .recv_matching(|frame| frame.contains("ib-uo-mark"))
        .await
        .expect("mark-read result");

    primary
        .send(&inbox_query_iq("ib-uo-q", Some(true), None, None))
        .await
        .expect("send unread-only query");
    let frames = collect_inbox_response(&mut primary, "ib-uo-q").await;
    assert!(
        entries_from_response(&frames).is_empty(),
        "unread-only must hide cleared conversations: {frames:?}"
    );
    let fin = parse_fin(frames.last().expect("fin"));
    assert_eq!(
        fin.attr("total"),
        Some("0"),
        "unread-only filter drives total=0"
    );

    let _ = primary.close().await;
    let _ = peer.close().await;
}

#[tokio::test]
async fn messages_false_elides_mam_result_payload() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[(PEER_USER, PEER_PASSWORD)]);
    let mut primary = primary_client(&server, "ib-nm").await;
    let mut peer = peer_client(&server, "ib-nm-peer").await;

    let primary_bare = format!("{PRIMARY_USER}@{DOMAIN}");
    send_dm(
        &mut peer,
        &mut primary,
        &primary_bare,
        "msg-nm-1",
        "bodyless",
    )
    .await;
    tokio::time::sleep(std::time::Duration::from_millis(100)).await;

    primary
        .send(&inbox_query_iq("ib-nm-q", None, Some(false), None))
        .await
        .expect("send messages=false query");
    let frames = collect_inbox_response(&mut primary, "ib-nm-q").await;
    let entries = entries_from_response(&frames);
    assert_eq!(entries.len(), 1, "still emits the entry: {frames:?}");
    let results = mam_results_from_response(&frames);
    assert!(
        results.is_empty(),
        "messages=false must elide all MAM <result/> payloads: {frames:?}"
    );

    let _ = primary.close().await;
    let _ = peer.close().await;
}

#[tokio::test]
async fn rsm_max_pages_response_and_fin_carries_cursor() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[
        (PEER_USER, PEER_PASSWORD),
        ("peer2", "peer2-password"),
        ("peer3", "peer3-password"),
    ]);
    let mut primary = primary_client(&server, "ib-rsm").await;
    let mut peer1 = peer_client(&server, "ib-rsm-p1").await;
    let mut peer2 = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "peer2",
        "peer2-password",
        "ib-rsm-p2",
    )
    .await
    .expect("connect peer2");
    let mut peer3 = WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "peer3",
        "peer3-password",
        "ib-rsm-p3",
    )
    .await
    .expect("connect peer3");

    let primary_bare = format!("{PRIMARY_USER}@{DOMAIN}");
    send_dm(&mut peer1, &mut primary, &primary_bare, "rsm-1", "one").await;
    send_dm(&mut peer2, &mut primary, &primary_bare, "rsm-2", "two").await;
    send_dm(&mut peer3, &mut primary, &primary_bare, "rsm-3", "three").await;
    tokio::time::sleep(std::time::Duration::from_millis(200)).await;

    primary
        .send(&inbox_query_iq("ib-rsm-q", None, Some(false), Some(2)))
        .await
        .expect("send paged query");
    let frames = collect_inbox_response(&mut primary, "ib-rsm-q").await;
    let entries = entries_from_response(&frames);
    assert_eq!(entries.len(), 2, "max=2 returns 2 entries: {frames:?}");
    let fin = parse_fin(frames.last().expect("fin"));
    assert_eq!(
        fin.attr("total"),
        Some("2"),
        "page total matches entry count"
    );
    let set = fin
        .children()
        .find(|c| c.name() == "set" && c.ns() == NS_RSM)
        .expect("fin carries RSM set when client asked for paging");
    assert!(
        set.children()
            .any(|c| c.name() == "count" && c.ns() == NS_RSM),
        "RSM set has <count/>: {fin:?}"
    );

    let _ = primary.close().await;
    let _ = peer1.close().await;
    let _ = peer2.close().await;
    let _ = peer3.close().await;
}

#[tokio::test]
async fn xep0333_displayed_marker_decrements_unread() {
    let _guard = TEST_SERIAL.lock().await;
    let server = TestServer::start_with_extra_accounts(&[(PEER_USER, PEER_PASSWORD)]);
    let mut primary = primary_client(&server, "ib-mk").await;
    let mut peer = peer_client(&server, "ib-mk-peer").await;

    // Use a MUC room so the XEP-0333 → MAM → mark_read bridge fires.
    let room = format!("inbox-mk-{}@muc.{DOMAIN}", uuid::Uuid::new_v4());
    primary
        .send(&format!(
            r#"<presence to='{room}/{PRIMARY_USER}'><x xmlns='http://jabber.org/protocol/muc'/></presence>"#
        ))
        .await
        .expect("join muc");
    primary
        .recv_until(|frame| frame.contains("<subject"))
        .await
        .expect("muc joined");

    let msg_id = "mk-msg-1";
    primary
        .send(&format!(
            r#"<message type='groupchat' to='{room}' id='{msg_id}'><body>seen me</body></message>"#
        ))
        .await
        .expect("send groupchat");
    primary
        .recv_matching(|frame| frame.contains(msg_id))
        .await
        .expect("groupchat echo");

    // Peer joins later so the MUC has another occupant for the displayed
    // bridge to act on; not strictly required but mirrors the production
    // shape.
    peer.send(&format!(
        r#"<presence to='{room}/{PEER_USER}'><x xmlns='http://jabber.org/protocol/muc'/></presence>"#
    ))
    .await
    .expect("peer join");

    // Send the XEP-0333 displayed marker from the primary user so the
    // server's interpreter arm runs `mark_inbox_read_from_displayed`.
    primary
        .send(&format!(
            r#"<message type='groupchat' to='{room}'>
                <displayed xmlns='{NS_CHAT_MARKERS}' id='{msg_id}'/>
            </message>"#
        ))
        .await
        .expect("send displayed marker");
    tokio::time::sleep(std::time::Duration::from_millis(250)).await;

    primary
        .send(&inbox_query_iq("ib-mk-q", None, Some(false), None))
        .await
        .expect("send inbox query");
    let frames = collect_inbox_response(&mut primary, "ib-mk-q").await;
    let fin = parse_fin(frames.last().expect("fin"));
    assert_eq!(
        fin.attr("unread"),
        Some("0"),
        "displayed marker should clear unread before the next query: {fin:?}"
    );

    let _ = primary.close().await;
    let _ = peer.close().await;
}
