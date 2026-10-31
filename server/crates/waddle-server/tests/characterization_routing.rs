//! W1.6b (#2046) characterization: local routing, overload, per-stream inbound
//! order and the connection-to-connection wait cycle.
//!
//! Every test pins what the server does today on the **local route**: one
//! spawned `waddle-server` (SQLite, clustering not configured; the shared
//! `waddle_ws_test_support::TestServer` harness) and real RFC 7395 WebSocket
//! clients. A characterization test passes on unchanged production code.
//! Where the pinned behaviour is one a later #2046 slice changes, the test says
//! so in a `Characterizes current behaviour; changes with ...` doc line, and
//! that slice updates the test deliberately.
//!
//! The remote route (a recipient on another node, including the deferred
//! ordered-relay IQ/presence handoff of
//! `clustering/route_bridge/delivery/mod.rs:60-63`) is [A] and belongs in the
//! two-process harness; it is not exercised here.
//!
//! # Exhausting a connection's outbound capacity without a seam
//!
//! Stanzas routed to a connection go through its bounded outbound channel
//! (`OUTBOUND_CHANNEL_SIZE` = 256, `server/routes/websocket/connection.rs`).
//! The connection loop stops draining that channel while its XEP-0198 send
//! window is paused (#1219): once 80% of the 1000-stanza unacked queue is
//! outstanding, the loop gates `outbound_rx` until the client acks down to the
//! low watermark, while still handling the client's own inbound stanzas. A
//! recipient that enabled resumable stream management and does not ack
//! therefore holds exactly [`SEND_WINDOW_HIGH`] written stanzas plus a full
//! [`OUTBOUND_CAPACITY`]-slot channel, and the next producer finds the channel
//! full. That exhaustion uses production behaviour. The cycle test adds a
//! `test-support` observer of the real IQ send future to prove both waits
//! entered before recovery ACKs; it changes no scheduling or deadline. Two
//! production deadlines bound the state and every test stays inside them: the
//! loop-level pause closes the stream into detach-for-resume 15 s after the
//! pause began (`SEND_WINDOW_LOOP_PAUSE_DEADLINE`), and a stanza handler is
//! cancelled after 15 s (`STANZA_HANDLER_WEDGE_TIMEOUT`).
//!
//! # Timing
//!
//! No sleep is used as synchronisation. Order is proven with in-stream
//! markers (an IQ forwarded through the same outbound channel after the
//! stanza under test) and XEP-0199 barriers. Timeouts are failure detectors,
//! except [`HOLD_WINDOW`], an absence window showing a stream is held: on a
//! very slow host it can pass when it should fail, never the reverse. Two
//! tests take real time because they retain the production deadlines: the cycle
//! test waits for the 15 s wedge backstop, and the dropped-DM test waits for
//! the ingress authority's background redelivery across maintenance passes.

use std::time::Duration;

use jid::{BareJid, FullJid, Jid};
use minidom::Element;
use tokio::time::Instant;
use waddle_ws_test_support::{raw_ws, TestServer, WsXmppClient};
use waddle_xmpp::stream_management::{DEFAULT_MAX_UNACKED_QUEUE_SIZE, SM_NS};
use xmpp_parsers::delay::Delay;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};
use xmpp_parsers::ns;
use xmpp_parsers::ping::Ping;
use xmpp_parsers::presence::{Presence, Type as PresenceType};
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};
use xmpp_parsers::stanza_id::StanzaId;

const DOMAIN: &str = "localhost";
const ALICE: (&str, &str) = ("alice", "characterization-routing-alice");
const BOB: (&str, &str) = ("bob", "characterization-routing-bob");
const CAROL: (&str, &str) = ("carol", "characterization-routing-carol");
const ACCOUNTS: [(&str, &str); 3] = [ALICE, BOB, CAROL];

/// Per-frame failure detector for ordinary reads.
const RECV: Duration = Duration::from_secs(10);
/// Absence window that shows a stream is held behind a blocked handler.
const HOLD_WINDOW: Duration = Duration::from_secs(1);
/// Failure detector for the ingress authority's background redelivery.
const INGRESS_REDELIVERY_BUDGET: Duration = Duration::from_secs(180);
/// XEP-0198 send-window high watermark: 80% of the unacked queue
/// (`send_window_high_for`, `waddle-xmpp/src/stream_management/state.rs`).
const SEND_WINDOW_HIGH: usize = DEFAULT_MAX_UNACKED_QUEUE_SIZE / 5 * 4;
/// `OUTBOUND_CHANNEL_SIZE` (`server/routes/websocket/connection.rs`).
const OUTBOUND_CAPACITY: usize = 256;

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

/// One parsed top-level frame from the server.
#[derive(Debug)]
enum Frame {
    Iq(Box<Iq>),
    Message(Box<Message>),
    Presence(Box<Presence>),
    /// XEP-0198 §4 `<r/>`.
    SmRequest,
}

fn classify(element: Element) -> Frame {
    if element.is("r", SM_NS) {
        return Frame::SmRequest;
    }
    if element.is("a", SM_NS) {
        panic!("unexpected XEP-0198 <a/> outside the cycle reader: {element:?}");
    }
    if element.is("iq", ns::DEFAULT_NS) {
        return Frame::Iq(Box::new(Iq::try_from(element).expect("typed IQ")));
    }
    if element.is("message", ns::DEFAULT_NS) {
        return Frame::Message(Box::new(Message::try_from(element).expect("typed message")));
    }
    if element.is("presence", ns::DEFAULT_NS) {
        return Frame::Presence(Box::new(
            Presence::try_from(element).expect("typed presence"),
        ));
    }
    panic!("unexpected top-level frame: {element:?}");
}

fn into_iq(frame: Frame, expected: &str, who: &str) -> Iq {
    match frame {
        Frame::Iq(iq) => *iq,
        other => panic!("{who}: expected IQ {expected}, got {other:?}"),
    }
}

fn into_message(frame: Frame, expected: &str, who: &str) -> Message {
    match frame {
        Frame::Message(message) => *message,
        other => panic!("{who}: expected message {expected}, got {other:?}"),
    }
}

fn into_presence(frame: Frame, expected: &str, who: &str) -> Presence {
    match frame {
        Frame::Presence(presence) => *presence,
        other => panic!("{who}: expected presence {expected}, got {other:?}"),
    }
}

#[derive(Debug, thiserror::Error)]
enum FrameReadError {
    #[error("timeout waiting for an XMPP frame")]
    Timeout,
    #[error("{0}")]
    Transport(String),
    #[error("unparseable frame: {0}")]
    Xml(String),
    #[error("unexpected WebSocket frame: {0:?}")]
    Unexpected(raw_ws::Message),
    #[error("WebSocket stream ended")]
    Ended,
}

/// Next XMPP element within `budget`. WebSocket control frames are skipped;
/// polling lets the client answer the server's RFC 7395 keepalive pings.
async fn next_element(
    client: &mut WsXmppClient,
    budget: Duration,
) -> Result<Element, FrameReadError> {
    // Own the deadline so an absence check cannot mistake a String-based
    // harness transport error for a timeout. The inner deadline is longer.
    tokio::time::timeout(budget, async {
        loop {
            match client
                .recv_raw_timeout(budget + RECV)
                .await
                .map_err(FrameReadError::Transport)?
            {
                Some(raw_ws::Message::Text(text)) => {
                    return text.as_str().parse().map_err(|error| {
                        FrameReadError::Xml(format!("{}: {error}", text.as_str()))
                    });
                }
                Some(raw_ws::Message::Ping(_) | raw_ws::Message::Pong(_)) => {}
                Some(other) => return Err(FrameReadError::Unexpected(other)),
                None => return Err(FrameReadError::Ended),
            }
        }
    })
    .await
    .map_err(|_| FrameReadError::Timeout)?
}

async fn next_frame(client: &mut WsXmppClient, budget: Duration) -> Result<Frame, FrameReadError> {
    next_element(client, budget).await.map(classify)
}

/// Next frame other than the server's XEP-0198 `<r/>` requests, which it
/// sends every few stanzas and on a send-window pause.
async fn next_stanza_within(
    client: &mut WsXmppClient,
    budget: Duration,
) -> Result<Frame, FrameReadError> {
    let deadline = Instant::now() + budget;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        match next_frame(client, remaining).await? {
            Frame::SmRequest => {}
            frame => return Ok(frame),
        }
    }
}

async fn next_stanza(client: &mut WsXmppClient, who: &str) -> Frame {
    next_stanza_within(client, RECV)
        .await
        .unwrap_or_else(|error| panic!("{who}: {error}"))
}

async fn send(client: &mut WsXmppClient, element: impl Into<Element>) {
    let mut bytes = Vec::new();
    element
        .into()
        .write_to(&mut bytes)
        .expect("serialize stanza");
    client
        .send(&String::from_utf8(bytes).expect("UTF-8 stanza"))
        .await
        .expect("send stanza");
}

fn full(jid: &str) -> FullJid {
    jid.parse().expect("static full JID")
}

fn bare(jid: &str) -> BareJid {
    jid.parse().expect("static bare JID")
}

fn server_jid() -> Jid {
    DOMAIN.parse().expect("server JID")
}

async fn connect(
    server: &TestServer,
    (user, password): (&str, &str),
    resource: &str,
) -> (WsXmppClient, FullJid) {
    let client = WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, user, password, resource)
        .await
        .unwrap_or_else(|error| panic!("{user}/{resource} connection: {error}"));
    let bound: FullJid = client
        .full_jid
        .as_deref()
        .expect("bound JID")
        .parse()
        .expect("bound full JID");
    assert_eq!(
        bound,
        full(&format!("{user}@{DOMAIN}/{resource}")),
        "the server binds the requested resource"
    );
    (client, bound)
}

/// XEP-0199 ping (`<iq type='get'><ping/></iq>`) addressed to `to`.
fn ping(to: impl Into<Jid>, id: &str) -> Iq {
    let mut iq = Iq::from_get(id, Ping);
    *iq.to_mut() = Some(to.into());
    iq
}

fn chat(to: impl Into<Jid>, id: &str) -> Message {
    let mut message = Message::new(Some(to.into()));
    message.id = Some(Id(id.to_owned()));
    message.type_ = MessageType::Chat;
    message.bodies.insert(Lang::new(), id.to_owned());
    message
}

/// RFC 6121 §4.2 initial presence / §4.4 presence update with a priority.
fn available_with_priority(priority: i8) -> Presence {
    Presence::new(PresenceType::None).with_priority(priority)
}

/// RFC 6121 §4.6 directed presence.
fn directed_presence(to: impl Into<Jid>, id: &str, type_: PresenceType) -> Presence {
    let mut presence = Presence::new(type_);
    presence.to = Some(to.into());
    presence.id = Some(id.to_owned());
    presence
}

fn sm_enable_resumable() -> Element {
    Element::builder("enable", SM_NS)
        .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
        .build()
}

fn sm_ack(handled: usize) -> Element {
    Element::builder("a", SM_NS)
        .attr(
            minidom::rxml::xml_ncname!("h").to_owned(),
            handled.to_string(),
        )
        .build()
}

/// XEP-0198 §3: enable resumable stream management.
async fn enable_resumable_sm(client: &mut WsXmppClient, who: &str) {
    send(client, sm_enable_resumable()).await;
    let enabled = match client.recv_raw_timeout(RECV).await {
        Ok(Some(raw_ws::Message::Text(text))) => {
            text.as_str().parse::<Element>().expect("<enabled/> XML")
        }
        other => panic!("{who}: expected <enabled/>, got {other:?}"),
    };
    assert!(enabled.is("enabled", SM_NS), "{who}: {enabled:?}");
    assert!(
        matches!(enabled.attr("resume"), Some("true" | "1")),
        "{who}: stream must be resumable for the send window to pace it: {enabled:?}"
    );
}

/// XEP-0199 round trip to the server: the connection handles one stream's
/// stanzas in order (RFC 6120 §10.1), so the pong proves everything sent
/// before it was processed. Returns the stanzas received before the pong.
async fn barrier(client: &mut WsXmppClient, id: &str) -> Vec<Frame> {
    send(client, ping(server_jid(), id)).await;
    let mut before = Vec::new();
    loop {
        let frame = next_stanza(client, id).await;
        if is_result_for(&frame, id) {
            return before;
        }
        before.push(frame);
    }
}

fn is_result_for(frame: &Frame, id: &str) -> bool {
    match frame {
        Frame::Iq(iq) => matches!(iq.as_ref(), Iq::Result { id: result, .. } if result == id),
        _ => false,
    }
}

async fn assert_quiet_barrier(client: &mut WsXmppClient, id: &str, who: &str) {
    let before = barrier(client, id).await;
    assert!(
        before.is_empty(),
        "{who} must receive nothing before its barrier {id}: {before:?}"
    );
}

/// Shows that `client`'s stream is held: nothing arrives for [`HOLD_WINDOW`].
async fn assert_held(client: &mut WsXmppClient, who: &str) {
    match next_frame(client, HOLD_WINDOW).await {
        Err(FrameReadError::Timeout) => {}
        Err(error) => panic!("{who}: absence check failed: {error}"),
        Ok(frame) => {
            panic!("{who}'s stream must be held behind its blocked stanza, got {frame:?}");
        }
    }
}

fn assert_forwarded_ping(frame: Frame, id: &str, from: &FullJid, to: &FullJid, who: &str) {
    match into_iq(frame, id, who) {
        Iq::Get {
            from: got_from,
            to: got_to,
            id: got_id,
            payload,
        } if got_id == id => {
            assert_eq!(got_from, Some(Jid::from(from.clone())), "{who}: {id} from");
            assert_eq!(got_to, Some(Jid::from(to.clone())), "{who}: {id} to");
            assert!(
                payload.is("ping", ns::PING),
                "{who}: {id} payload {payload:?}"
            );
        }
        other => panic!("{who}: expected forwarded ping {id}, got {other:?}"),
    }
}

/// Sends an in-stream marker: a ping forwarded through `to`'s outbound
/// channel after everything `sender` sent before it.
async fn send_marker(sender: &mut WsXmppClient, to: &FullJid, id: &str) {
    send(sender, ping(to.clone(), id)).await;
}

/// The marker is the next stanza: nothing the sender sent earlier was routed
/// to this connection.
async fn expect_marker(
    recipient: &mut WsXmppClient,
    id: &str,
    from: &FullJid,
    to: &FullJid,
    who: &str,
) {
    let frame = next_stanza(recipient, who).await;
    assert_forwarded_ping(frame, id, from, to, who);
}

fn assert_chat(frame: Frame, id: &str, from: &FullJid, to: &Jid, who: &str) -> Message {
    match into_message(frame, id, who) {
        message if message.id.as_ref().is_some_and(|got| got.0 == id) => {
            assert_eq!(message.type_, MessageType::Chat, "{who}: {id} type");
            assert_eq!(
                message.from,
                Some(Jid::from(from.clone())),
                "{who}: {id} from"
            );
            assert_eq!(message.to.as_ref(), Some(to), "{who}: {id} to");
            assert_eq!(
                message.bodies.get(&Lang::new()).map(String::as_str),
                Some(id),
                "{who}: {id} body"
            );
            message
        }
        other => panic!("{who}: expected chat {id}, got {other:?}"),
    }
}

fn assert_presence(frame: Frame, id: &str, from: &FullJid, type_: PresenceType, who: &str) {
    match into_presence(frame, id, who) {
        presence if presence.id.as_deref() == Some(id) => {
            assert_eq!(
                presence.from,
                Some(Jid::from(from.clone())),
                "{who}: {id} from"
            );
            assert_eq!(presence.type_, type_, "{who}: {id} type");
        }
        other => panic!("{who}: expected presence {id}, got {other:?}"),
    }
}

fn delays(message: &Message) -> Vec<Delay> {
    message
        .payloads
        .iter()
        .filter(|payload| payload.is("delay", ns::DELAY))
        .map(|payload| Delay::try_from(payload.clone()).expect("typed XEP-0203 delay"))
        .collect()
}

fn stanza_id_issuers(message: &Message) -> Vec<Jid> {
    message
        .payloads
        .iter()
        .filter(|payload| payload.is("stanza-id", ns::SID))
        .map(|payload| {
            StanzaId::try_from(payload.clone())
                .expect("typed XEP-0359 stanza-id")
                .by
        })
        .collect()
}

/// XEP-0203 §3 stamp of RFC 6121 §8.5.2.2 offline storage, as the server
/// writes it when it flushes stored messages at initial presence.
fn assert_offline_storage_delay(message: &Message, who: &str) {
    let delays = delays(message);
    assert_eq!(
        delays.len(),
        1,
        "{who}: one offline-storage delay: {message:?}"
    );
    assert_eq!(delays[0].from, Some(server_jid()), "{who}: delay from");
    assert_eq!(
        delays[0].data.as_deref(),
        Some("Offline Storage"),
        "{who}: delay reason"
    );
}

fn assert_iq_error(
    frame: Frame,
    id: &str,
    from: Jid,
    to: Option<Jid>,
    type_: ErrorType,
    condition: DefinedCondition,
    who: &str,
) -> Option<Element> {
    match into_iq(frame, id, who) {
        Iq::Error {
            from: got_from,
            to: got_to,
            id: got_id,
            error,
            payload,
        } if got_id == id => {
            assert_eq!(got_from, Some(from), "{who}: {id} error from");
            assert_eq!(got_to, to, "{who}: {id} error to");
            assert_eq!(error.type_, type_, "{who}: {id} error type");
            assert_eq!(error.defined_condition, condition, "{who}: {id} condition");
            payload
        }
        other => panic!("{who}: expected IQ error {id}, got {other:?}"),
    }
}

fn fill_id(index: usize) -> String {
    format!("fill-{index}")
}

async fn expect_fills(
    recipient: &mut WsXmppClient,
    filler: &FullJid,
    recipient_jid: &FullJid,
    range: std::ops::Range<usize>,
) {
    for index in range {
        let frame = next_stanza(recipient, "saturated recipient").await;
        assert_forwarded_ping(
            frame,
            &fill_id(index),
            filler,
            recipient_jid,
            "saturated recipient",
        );
    }
}

/// Drives `recipient`'s connection to an exactly full outbound channel that
/// nothing drains (see the module docs). `recipient` must have resumable SM
/// enabled and must not ack. Afterwards the caller has under 15 s to ack
/// (`sm_ack(SEND_WINDOW_HIGH)`) before the loop-level pause deadline detaches
/// the stream.
///
/// 1. `filler` forwards [`SEND_WINDOW_HIGH`] pings; `recipient` reads them in
///    order. The last one latches the pause, so the loop stops polling its
///    outbound channel.
/// 2. `filler` forwards [`OUTBOUND_CAPACITY`] more; they fill the channel.
/// 3. `filler`'s barrier returns with nothing before it: none of those sends
///    waited, so the channel took them all and is now full.
async fn saturate(
    filler: &mut WsXmppClient,
    filler_jid: &FullJid,
    recipient: &mut WsXmppClient,
    recipient_jid: &FullJid,
) {
    for index in 0..SEND_WINDOW_HIGH {
        send(filler, ping(recipient_jid.clone(), &fill_id(index))).await;
    }
    expect_fills(recipient, filler_jid, recipient_jid, 0..SEND_WINDOW_HIGH).await;
    for index in SEND_WINDOW_HIGH..SEND_WINDOW_HIGH + OUTBOUND_CAPACITY {
        send(filler, ping(recipient_jid.clone(), &fill_id(index))).await;
    }
    assert_quiet_barrier(filler, "filler-saturated", "filler").await;
}

// ---------------------------------------------------------------------------
// Local routing
// ---------------------------------------------------------------------------

/// RFC 6121 §8.5.3.1: a chat message to a full JID reaches that resource only.
/// RFC 6121 §8.5.3.2.1: to an unbound resource of an existing account it is
/// handled as if sent to the bare JID (the highest-priority available
/// resource gets it, with the original `to` kept). The sender sees nothing.
#[tokio::test]
async fn full_jid_chat_reaches_only_that_resource_and_an_unbound_resource_falls_back_to_bare_selection(
) {
    let server = TestServer::start_with_extra_accounts(&ACCOUNTS);
    let (mut web, web_jid) = connect(&server, BOB, "web").await;
    send(&mut web, available_with_priority(5)).await;
    assert_quiet_barrier(&mut web, "web-available", "web").await;
    let (mut phone, phone_jid) = connect(&server, BOB, "phone").await;
    send(&mut phone, available_with_priority(1)).await;
    assert_quiet_barrier(&mut phone, "phone-available", "phone").await;
    let (mut alice, alice_jid) = connect(&server, ALICE, "desk").await;

    send(&mut alice, chat(phone_jid.clone(), "dm-to-phone")).await;
    send_marker(&mut alice, &web_jid, "web-marker-1").await;
    send_marker(&mut alice, &phone_jid, "phone-marker-1").await;
    let frame = next_stanza(&mut phone, "phone").await;
    assert_chat(
        frame,
        "dm-to-phone",
        &alice_jid,
        &phone_jid.clone().into(),
        "phone",
    );
    expect_marker(
        &mut phone,
        "phone-marker-1",
        &alice_jid,
        &phone_jid,
        "phone",
    )
    .await;
    expect_marker(&mut web, "web-marker-1", &alice_jid, &web_jid, "web").await;

    let unbound = full("bob@localhost/unbound");
    send(&mut alice, chat(unbound.clone(), "dm-to-unbound")).await;
    send_marker(&mut alice, &web_jid, "web-marker-2").await;
    send_marker(&mut alice, &phone_jid, "phone-marker-2").await;
    let frame = next_stanza(&mut web, "web").await;
    assert_chat(frame, "dm-to-unbound", &alice_jid, &unbound.into(), "web");
    expect_marker(&mut web, "web-marker-2", &alice_jid, &web_jid, "web").await;
    expect_marker(
        &mut phone,
        "phone-marker-2",
        &alice_jid,
        &phone_jid,
        "phone",
    )
    .await;

    assert_quiet_barrier(&mut alice, "alice-done", "alice").await;
}

/// Waddle's policy among the options RFC 6121 §8.5.2.1.1 permits: a chat
/// message to a bare JID goes to the available resources with the highest
/// non-negative priority, all of them on a tie.
/// When every available resource has negative priority nothing is delivered
/// live and the sender sees nothing; the message is stored (RFC 6121
/// §8.5.2.2) and flushed with an XEP-0203 offline-storage stamp to the next
/// session that sends initial presence.
#[tokio::test]
async fn bare_jid_chat_selects_highest_non_negative_priority_and_stores_when_all_are_negative() {
    let server = TestServer::start_with_extra_accounts(&ACCOUNTS);
    let bob = bare("bob@localhost");
    let (mut web, web_jid) = connect(&server, BOB, "web").await;
    send(&mut web, available_with_priority(5)).await;
    assert_quiet_barrier(&mut web, "web-available", "web").await;
    let (mut phone, phone_jid) = connect(&server, BOB, "phone").await;
    send(&mut phone, available_with_priority(1)).await;
    assert_quiet_barrier(&mut phone, "phone-available", "phone").await;
    let (mut alice, alice_jid) = connect(&server, ALICE, "desk").await;

    send(&mut alice, chat(bob.clone(), "dm-highest")).await;
    send_marker(&mut alice, &web_jid, "web-marker-1").await;
    send_marker(&mut alice, &phone_jid, "phone-marker-1").await;
    let frame = next_stanza(&mut web, "web").await;
    assert_chat(frame, "dm-highest", &alice_jid, &bob.clone().into(), "web");
    expect_marker(&mut web, "web-marker-1", &alice_jid, &web_jid, "web").await;
    expect_marker(
        &mut phone,
        "phone-marker-1",
        &alice_jid,
        &phone_jid,
        "phone",
    )
    .await;

    send(&mut phone, available_with_priority(5)).await;
    assert_quiet_barrier(&mut phone, "phone-tied", "phone").await;
    send(&mut alice, chat(bob.clone(), "dm-tied")).await;
    send_marker(&mut alice, &web_jid, "web-marker-2").await;
    send_marker(&mut alice, &phone_jid, "phone-marker-2").await;
    for (client, jid, marker, who) in [
        (&mut web, &web_jid, "web-marker-2", "web"),
        (&mut phone, &phone_jid, "phone-marker-2", "phone"),
    ] {
        let frame = next_stanza(client, who).await;
        assert_chat(frame, "dm-tied", &alice_jid, &bob.clone().into(), who);
        expect_marker(client, marker, &alice_jid, jid, who).await;
    }

    send(&mut web, available_with_priority(-1)).await;
    assert_quiet_barrier(&mut web, "web-negative", "web").await;
    send(&mut phone, available_with_priority(-1)).await;
    assert_quiet_barrier(&mut phone, "phone-negative", "phone").await;
    send(&mut alice, chat(bob.clone(), "dm-all-negative")).await;
    send_marker(&mut alice, &web_jid, "web-marker-3").await;
    send_marker(&mut alice, &phone_jid, "phone-marker-3").await;
    expect_marker(&mut web, "web-marker-3", &alice_jid, &web_jid, "web").await;
    expect_marker(
        &mut phone,
        "phone-marker-3",
        &alice_jid,
        &phone_jid,
        "phone",
    )
    .await;
    assert_quiet_barrier(&mut alice, "alice-after-negative", "alice").await;

    web.close().await.expect("close web");
    phone.close().await.expect("close phone");
    let (mut laptop, _) = connect(&server, BOB, "laptop").await;
    send(&mut laptop, available_with_priority(0)).await;
    let frame = next_stanza(&mut laptop, "laptop").await;
    let flushed = assert_chat(frame, "dm-all-negative", &alice_jid, &bob.into(), "laptop");
    assert_offline_storage_delay(&flushed, "laptop");
}

/// RFC 6121 §8.5.2.2 / XEP-0160: with no session online, chat messages to the
/// bare and to a full JID are stored without any error to the sender. A new
/// session receives nothing while it is only bound (RFC 6121 §4.2: not yet
/// available) and both messages, in send order and with an XEP-0203
/// offline-storage stamp, after its initial presence.
#[tokio::test]
async fn chat_to_an_offline_account_is_stored_and_flushed_in_order_at_initial_presence() {
    let server = TestServer::start_with_extra_accounts(&ACCOUNTS);
    let bob = bare("bob@localhost");
    let bob_web = full("bob@localhost/web");
    let (mut alice, alice_jid) = connect(&server, ALICE, "desk").await;
    send(&mut alice, chat(bob.clone(), "offline-to-bare")).await;
    send(&mut alice, chat(bob_web.clone(), "offline-to-full")).await;
    assert_quiet_barrier(&mut alice, "alice-after-offline", "alice").await;

    let (mut web, web_jid) = connect(&server, BOB, "web").await;
    send_marker(&mut alice, &web_jid, "bound-marker").await;
    expect_marker(&mut web, "bound-marker", &alice_jid, &web_jid, "web").await;

    send(&mut web, available_with_priority(0)).await;
    let frame = next_stanza(&mut web, "web").await;
    let first = assert_chat(frame, "offline-to-bare", &alice_jid, &bob.into(), "web");
    assert_offline_storage_delay(&first, "web");
    let frame = next_stanza(&mut web, "web").await;
    let second = assert_chat(frame, "offline-to-full", &alice_jid, &bob_web.into(), "web");
    assert_offline_storage_delay(&second, "web");
    send_marker(&mut alice, &web_jid, "available-marker").await;
    expect_marker(&mut web, "available-marker", &alice_jid, &web_jid, "web").await;
}

/// IQ routing on the local route:
/// - a get to a bound full JID is forwarded verbatim with `from` stamped;
/// - a get to an unbound resource or to a resource of an unknown account is
///   answered with `service-unavailable`/`cancel` from that JID (RFC 6121
///   §8.5.3.2.3 and §8.5.1), echoing the request payload;
/// - a ping to a bare JID is answered by the server on behalf of the account
///   (RFC 6120 §10.5.3.2), with no `to`;
/// - a chat message to an unknown local account bounces with
///   `service-unavailable`/`cancel` (RFC 6121 §8.5.1).
#[tokio::test]
async fn iq_to_full_jid_is_forwarded_or_bounced_and_unknown_accounts_bounce_service_unavailable() {
    let server = TestServer::start_with_extra_accounts(&ACCOUNTS);
    let (mut web, web_jid) = connect(&server, BOB, "web").await;
    let (mut alice, alice_jid) = connect(&server, ALICE, "desk").await;
    let alice_full: Jid = alice_jid.clone().into();

    send(&mut alice, ping(web_jid.clone(), "iq-to-bound")).await;
    let frame = next_stanza(&mut web, "web").await;
    assert_forwarded_ping(frame, "iq-to-bound", &alice_jid, &web_jid, "web");

    for (target, id) in [
        (full("bob@localhost/unbound"), "iq-to-unbound"),
        (full("nobody@localhost/anywhere"), "iq-to-unknown-account"),
    ] {
        send(&mut alice, ping(target.clone(), id)).await;
        let frame = next_stanza(&mut alice, "alice").await;
        let payload = assert_iq_error(
            frame,
            id,
            target.into(),
            Some(alice_full.clone()),
            ErrorType::Cancel,
            DefinedCondition::ServiceUnavailable,
            "alice",
        );
        assert!(
            payload.is_some_and(|payload| payload.is("ping", ns::PING)),
            "{id}: the bounce echoes the request payload"
        );
    }

    send(&mut alice, ping(bare("bob@localhost"), "iq-to-bare")).await;
    let frame = next_stanza(&mut alice, "alice").await;
    match into_iq(frame, "iq-to-bare", "alice") {
        Iq::Result {
            from,
            to,
            id,
            payload,
        } if id == "iq-to-bare" => {
            assert_eq!(from, Some(bare("bob@localhost").into()));
            assert_eq!(
                to, None,
                "the server's reply on behalf of the account has no to"
            );
            assert_eq!(payload, None);
        }
        other => panic!("alice: expected the server's ping result, got {other:?}"),
    }

    let nobody = bare("nobody@localhost");
    send(&mut alice, chat(nobody.clone(), "dm-to-unknown-account")).await;
    let frame = next_stanza(&mut alice, "alice").await;
    match into_message(frame, "dm-to-unknown-account", "alice") {
        message
            if message
                .id
                .as_ref()
                .is_some_and(|id| id.0 == "dm-to-unknown-account") =>
        {
            assert_eq!(message.type_, MessageType::Error);
            assert_eq!(message.from, Some(nobody.into()));
            let errors: Vec<StanzaError> = message
                .payloads
                .iter()
                .filter(|payload| payload.is("error", ns::DEFAULT_NS))
                .map(|payload| StanzaError::try_from(payload.clone()).expect("typed error"))
                .collect();
            assert_eq!(errors.len(), 1, "{message:?}");
            assert_eq!(errors[0].type_, ErrorType::Cancel);
            assert_eq!(
                errors[0].defined_condition,
                DefinedCondition::ServiceUnavailable
            );
        }
        other => panic!("alice: expected the message bounce, got {other:?}"),
    }

    send_marker(&mut alice, &web_jid, "web-marker").await;
    expect_marker(&mut web, "web-marker", &alice_jid, &web_jid, "web").await;
}

/// RFC 6120 §8.2.3 / RFC 6121 §8.5.3.2.3: authenticated clients complete
/// real full-JID requests with a result and an error. Both replies keep their
/// IDs, payloads and destination, replace a forged `from` with the responder's
/// bound JID, and arrive in send order before the responder's later marker.
/// The responder receives no reply to either response. This pins #2070's
/// forwarding behavior alongside the other local routing characterizations.
#[tokio::test]
async fn iq_result_and_error_from_one_client_to_another_are_routed_in_order_with_authentic_identity(
) {
    let server = TestServer::start_with_extra_accounts(&ACCOUNTS);
    let (mut web, web_jid) = connect(&server, BOB, "web").await;
    let (mut alice, alice_jid) = connect(&server, ALICE, "desk").await;
    let payload: Element = Ping.into();

    let requests = [
        ping(web_jid.clone(), "request-result"),
        ping(web_jid.clone(), "request-error"),
    ];
    for request in &requests {
        send(&mut alice, request.clone()).await;
    }
    for mut expected in requests {
        *expected.from_mut() = Some(alice_jid.clone().into());
        let frame = next_stanza(&mut web, "web").await;
        assert_eq!(into_iq(frame, expected.id(), "web"), expected);
    }

    let forged_from = full("carol@localhost/forged");
    let replies = [
        Iq::Result {
            from: Some(forged_from.clone().into()),
            to: Some(alice_jid.clone().into()),
            id: "request-result".to_owned(),
            payload: Some(payload.clone()),
        },
        Iq::Error {
            from: Some(forged_from.into()),
            to: Some(alice_jid.clone().into()),
            id: "request-error".to_owned(),
            error: StanzaError::new(
                ErrorType::Cancel,
                DefinedCondition::FeatureNotImplemented,
                "en",
                "characterization",
            ),
            payload: Some(payload),
        },
    ];
    for reply in &replies {
        send(&mut web, reply.clone()).await;
    }
    send_marker(&mut web, &alice_jid, "web-marker").await;
    for mut expected in replies {
        *expected.from_mut() = Some(web_jid.clone().into());
        let frame = next_stanza(&mut alice, "alice").await;
        assert_eq!(into_iq(frame, expected.id(), "alice"), expected);
    }
    expect_marker(&mut alice, "web-marker", &web_jid, &alice_jid, "alice").await;
    assert_quiet_barrier(&mut web, "web-after-replies", "web").await;
}

// ---------------------------------------------------------------------------
// Per-stream inbound order
// ---------------------------------------------------------------------------

/// RFC 6120 §10.1: one stream's message, IQ and presence stanzas to the same
/// local recipient arrive in the order the sender sent them.
#[tokio::test]
async fn mixed_stanzas_from_one_stream_reach_the_recipient_in_send_order() {
    let server = TestServer::start_with_extra_accounts(&ACCOUNTS);
    let (mut web, web_jid) = connect(&server, BOB, "web").await;
    let (mut alice, alice_jid) = connect(&server, ALICE, "desk").await;
    let web_full: Jid = web_jid.clone().into();

    for round in 0..4 {
        send(&mut alice, chat(web_jid.clone(), &format!("chat-{round}"))).await;
        send(&mut alice, ping(web_jid.clone(), &format!("iq-{round}"))).await;
        send(
            &mut alice,
            directed_presence(
                web_jid.clone(),
                &format!("presence-{round}"),
                PresenceType::None,
            ),
        )
        .await;
    }
    for round in 0..4 {
        let frame = next_stanza(&mut web, "web").await;
        assert_chat(
            frame,
            &format!("chat-{round}"),
            &alice_jid,
            &web_full,
            "web",
        );
        let frame = next_stanza(&mut web, "web").await;
        assert_forwarded_ping(frame, &format!("iq-{round}"), &alice_jid, &web_jid, "web");
        let frame = next_stanza(&mut web, "web").await;
        assert_presence(
            frame,
            &format!("presence-{round}"),
            &alice_jid,
            PresenceType::None,
            "web",
        );
    }
    send_marker(&mut alice, &web_jid, "web-marker").await;
    expect_marker(&mut web, "web-marker", &alice_jid, &web_jid, "web").await;
}

/// Overload, IQ: an IQ forwarded into a full outbound channel waits for
/// capacity inside the sender's turn (`ConnectionRegistry::send_to`, called
/// from `handlers/iq/full_jid_forward.rs`). The sender sees no error, and its
/// later stanzas, including a ping to the server, are held behind it
/// (RFC 6120 §10.1 serial processing). When the recipient acks, its channel
/// drains in FIFO order, the held IQ follows the queued ones, and the
/// sender's later stanzas follow in send order.
///
/// Characterizes current behaviour; changes with D2d (#2046): its row
/// converts the IQ-forward producer to `try_send` with a typed `Overloaded`
/// disposition (B2d moves this call site behaviour-preserving).
#[tokio::test]
async fn iq_into_a_full_outbound_channel_waits_in_the_sender_turn_and_holds_its_later_stanzas() {
    let server = TestServer::start_with_extra_accounts(&ACCOUNTS);
    let (mut web, web_jid) = connect(&server, BOB, "web").await;
    enable_resumable_sm(&mut web, "web").await;
    let (mut carol, carol_jid) = connect(&server, CAROL, "filler").await;
    let (mut alice, alice_jid) = connect(&server, ALICE, "desk").await;
    saturate(&mut carol, &carol_jid, &mut web, &web_jid).await;

    send(&mut alice, ping(web_jid.clone(), "blocked-iq")).await;
    send(
        &mut alice,
        directed_presence(web_jid.clone(), "held-presence", PresenceType::None),
    )
    .await;
    send(&mut alice, ping(web_jid.clone(), "held-iq")).await;
    send(&mut alice, ping(server_jid(), "held-server-ping")).await;
    assert_held(&mut alice, "alice").await;

    send(&mut web, sm_ack(SEND_WINDOW_HIGH)).await;
    expect_fills(
        &mut web,
        &carol_jid,
        &web_jid,
        SEND_WINDOW_HIGH..SEND_WINDOW_HIGH + OUTBOUND_CAPACITY,
    )
    .await;
    let frame = next_stanza(&mut web, "web").await;
    assert_forwarded_ping(frame, "blocked-iq", &alice_jid, &web_jid, "web");
    let frame = next_stanza(&mut web, "web").await;
    assert_presence(
        frame,
        "held-presence",
        &alice_jid,
        PresenceType::None,
        "web",
    );
    let frame = next_stanza(&mut web, "web").await;
    assert_forwarded_ping(frame, "held-iq", &alice_jid, &web_jid, "web");

    let frame = next_stanza(&mut alice, "alice").await;
    match into_iq(frame, "held-server-ping", "alice") {
        Iq::Result { id, .. } if id == "held-server-ping" => {}
        other => panic!("alice: the held server ping is answered with no error first: {other:?}"),
    }
}

/// Per-stream order with a slow presence handler: directed presence into a
/// full outbound channel waits in the sender's turn
/// (`handlers/presence/subscription/directed.rs`, `send_to`), holding the
/// sender's later IQ and presence until the recipient's channel drains; all
/// three then arrive in send order.
///
/// Characterizes current behaviour; changes with D2c (#2046): its row
/// converts the roster/presence/subscription producers to `try_send` with a
/// typed `Overloaded` disposition.
#[tokio::test]
async fn presence_into_a_full_outbound_channel_holds_the_sender_stream_in_order() {
    let server = TestServer::start_with_extra_accounts(&ACCOUNTS);
    let (mut web, web_jid) = connect(&server, BOB, "web").await;
    enable_resumable_sm(&mut web, "web").await;
    let (mut carol, carol_jid) = connect(&server, CAROL, "filler").await;
    let (mut alice, alice_jid) = connect(&server, ALICE, "desk").await;
    saturate(&mut carol, &carol_jid, &mut web, &web_jid).await;

    send(
        &mut alice,
        directed_presence(web_jid.clone(), "blocked-presence", PresenceType::None),
    )
    .await;
    send(&mut alice, ping(web_jid.clone(), "held-iq")).await;
    send(
        &mut alice,
        directed_presence(
            web_jid.clone(),
            "held-unavailable",
            PresenceType::Unavailable,
        ),
    )
    .await;
    send(&mut alice, ping(server_jid(), "held-server-ping")).await;
    assert_held(&mut alice, "alice").await;

    send(&mut web, sm_ack(SEND_WINDOW_HIGH)).await;
    expect_fills(
        &mut web,
        &carol_jid,
        &web_jid,
        SEND_WINDOW_HIGH..SEND_WINDOW_HIGH + OUTBOUND_CAPACITY,
    )
    .await;
    let frame = next_stanza(&mut web, "web").await;
    assert_presence(
        frame,
        "blocked-presence",
        &alice_jid,
        PresenceType::None,
        "web",
    );
    let frame = next_stanza(&mut web, "web").await;
    assert_forwarded_ping(frame, "held-iq", &alice_jid, &web_jid, "web");
    let frame = next_stanza(&mut web, "web").await;
    assert_presence(
        frame,
        "held-unavailable",
        &alice_jid,
        PresenceType::Unavailable,
        "web",
    );

    let frame = next_stanza(&mut alice, "alice").await;
    match into_iq(frame, "held-server-ping", "alice") {
        Iq::Result { id, .. } if id == "held-server-ping" => {}
        other => panic!("alice: the held server ping is answered with no error first: {other:?}"),
    }
}

/// Overload, chat: a DM whose recipient's outbound channel is full is not
/// delivered live (the ingress live delivery's non-blocking `try_send` gets
/// `DroppedFull` after its bounded retries) and the sender sees no error and
/// is not held. A later DM from the same sender to the recovered recipient is
/// not delivered live either: a marker forwarded after it arrives first. All
/// three DMs arrive later through the ingress authority's background
/// redelivery, in send order, with recipient stanza-ids and without an
/// XEP-0203 delay stamp. Each row has a 60 s eligibility grace; the later DM
/// can need a separate 27–33 s maintenance tick, so all three reads share one
/// bounded redelivery deadline rather than assuming a short inter-message gap.
/// The next recipient-side marker bounds the duplicate check; this does not
/// establish that no duplicate can arrive in a future maintenance pass.
///
/// Characterizes current behaviour; changes with: no #2046 slice row names
/// this path. D2b-e convert producers that await capacity, and this one
/// already uses `try_send`. C2h, C6c and ACT-LANES are [A] and change only
/// lane-carried DMs. The W1.9 inventory has to place it (see the W1.6b
/// notes).
#[tokio::test]
async fn dm_into_a_full_outbound_channel_is_not_delivered_live_and_later_dms_wait_for_ingress_redelivery(
) {
    let server = TestServer::start_with_extra_accounts(&ACCOUNTS);
    let bob = bare("bob@localhost");
    let (mut web, web_jid) = connect(&server, BOB, "web").await;
    send(&mut web, available_with_priority(0)).await;
    assert_quiet_barrier(&mut web, "web-available", "web").await;
    enable_resumable_sm(&mut web, "web").await;
    let (mut carol, carol_jid) = connect(&server, CAROL, "filler").await;
    let (mut alice, alice_jid) = connect(&server, ALICE, "desk").await;
    saturate(&mut carol, &carol_jid, &mut web, &web_jid).await;

    send(&mut alice, chat(web_jid.clone(), "dm-to-full-while-full")).await;
    send(&mut alice, chat(bob.clone(), "dm-to-bare-while-full")).await;
    assert_quiet_barrier(&mut alice, "alice-after-dropped", "alice").await;

    send(&mut web, sm_ack(SEND_WINDOW_HIGH)).await;
    expect_fills(
        &mut web,
        &carol_jid,
        &web_jid,
        SEND_WINDOW_HIGH..SEND_WINDOW_HIGH + OUTBOUND_CAPACITY,
    )
    .await;

    send(&mut alice, chat(web_jid.clone(), "dm-after-recovery")).await;
    send_marker(&mut alice, &web_jid, "after-recovery-marker").await;
    expect_marker(
        &mut web,
        "after-recovery-marker",
        &alice_jid,
        &web_jid,
        "web",
    )
    .await;

    let mut redelivered = Vec::new();
    let redelivery_deadline = Instant::now() + INGRESS_REDELIVERY_BUDGET;
    for (id, to) in [
        ("dm-to-full-while-full", Jid::from(web_jid.clone())),
        ("dm-to-bare-while-full", Jid::from(bob.clone())),
        ("dm-after-recovery", Jid::from(web_jid.clone())),
    ] {
        let frame = next_stanza_within(
            &mut web,
            redelivery_deadline.saturating_duration_since(Instant::now()),
        )
        .await
        .unwrap_or_else(|error| panic!("web: redelivery of {id}: {error}"));
        let message = assert_chat(frame, id, &alice_jid, &to, "web");
        assert!(
            delays(&message).is_empty(),
            "{id}: no delay stamp: {message:?}"
        );
        assert!(
            stanza_id_issuers(&message).contains(&Jid::from(bob.clone())),
            "{id}: recipient stanza-id: {message:?}"
        );
        redelivered.push(message);
    }
    assert_eq!(redelivered.len(), 3);
    send_marker(&mut alice, &web_jid, "redelivery-complete-marker").await;
    expect_marker(
        &mut web,
        "redelivery-complete-marker",
        &alice_jid,
        &web_jid,
        "web",
    )
    .await;
    assert_quiet_barrier(&mut alice, "alice-after-redelivery", "alice").await;
}

// The send observer exists only in test-support builds. The all-features
// publish gate requires the cycle test by its exact name.
#[cfg(feature = "test-support")]
#[path = "characterization_routing/cycle.rs"]
mod cycle;
