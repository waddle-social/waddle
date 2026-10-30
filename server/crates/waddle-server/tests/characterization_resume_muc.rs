//! W1.6c characterization (#2046): XEP-0198 resume and the XEP-0045 join burst.
//!
//! Pins what a WebSocket client observes today, on the local (single-process)
//! route, so the Wave 2 connection-actor slices change it only deliberately:
//!
//! - XEP-0045 §7.1 / §7.2.2: the join burst (occupant presence, self-presence,
//!   subject; no discussion history) is written before a live room message,
//!   including one queued while the burst is still unwritten.
//! - XEP-0198 §5: a resume after transport loss reports the inbound `h`,
//!   replays exactly the unacknowledged tail with a XEP-0203 delay, and keeps
//!   both counters running.
//! - XEP-0198 §5 with a peer delivering throughout the resume: every replayed
//!   frame precedes every live frame, in send order, exactly once.
//! - A canonical-read outage at detach: the unacknowledged ingress frame is
//!   still replayed, but without its keyed `sm_ingress_appends` proof.
//!
//! Tests in the `seams` module need the `test-support` feature: they arm the
//! deterministic seams in `server/routes/websocket/characterization_seams.rs`
//! through the spawned server's environment. Everything else runs on default
//! features.

use std::time::Duration;

use jid::{BareJid, FullJid};
use minidom::Element;
use waddle_server::sm_persistence::DatabaseSmPersistence;
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::muc::presence::{NS_MUC, NS_MUC_USER};
use waddle_xmpp::pending_delivery::SmSessionId;
use waddle_xmpp::stream_management::persistence::{
    IngressCustodyDisposition, PersistedIngressAppend, PersistedSession, SmPersistenceStorage,
};
use waddle_xmpp::stream_management::SM_NS;
use waddle_xmpp::xep::xep0203::NS_DELAY;
use xmpp_parsers::iq::Iq;
use xmpp_parsers::message::{Id, Lang, Message, MessageType};
use xmpp_parsers::ns::JABBER_CLIENT;
use xmpp_parsers::ping::Ping;
use xmpp_parsers::presence::{Presence, Type as PresenceType};

const DOMAIN: &str = "localhost";
const ALICE: (&str, &str) = ("alice", "characterization-resume-alice");
const BOB: (&str, &str) = ("bob", "characterization-resume-bob");
const ACCOUNTS: &[(&str, &str)] = &[ALICE, BOB];
/// Self-presence status code (XEP-0045 §7.2.2).
const SELF_PRESENCE_STATUS: &str = "110";

// ---------------------------------------------------------------------------
// Wire helpers
// ---------------------------------------------------------------------------

async fn send(client: &mut WsXmppClient, element: Element) {
    let mut bytes = Vec::new();
    element.write_to(&mut bytes).expect("serialize XML");
    client
        .send(&String::from_utf8(bytes).expect("XML UTF-8"))
        .await
        .expect("send XML");
}

async fn recv_element(client: &mut WsXmppClient) -> Element {
    client
        .recv()
        .await
        .expect("server frame")
        .parse()
        .expect("server XML")
}

/// The next frame that is not a server-initiated XEP-0198 `<r/>`; the server
/// may request an ack at any batch boundary, which is not what these pin.
async fn recv_skipping_ack_requests(client: &mut WsXmppClient) -> Element {
    loop {
        let element = recv_element(client).await;
        if !element.is("r", SM_NS) {
            return element;
        }
    }
}

async fn connect(server: &TestServer, account: (&str, &str), resource: &str) -> WsXmppClient {
    WsXmppClient::connect_and_auth(&server.ws_url(), DOMAIN, account.0, account.1, resource)
        .await
        .expect("connect, authenticate and bind")
}

async fn connect_owner(server: &TestServer) -> WsXmppClient {
    // The fixed `admin` account is a server owner, so it may create rooms.
    WsXmppClient::connect_and_auth(
        &server.ws_url(),
        DOMAIN,
        "admin",
        server.fixed_account_password(),
        "owner",
    )
    .await
    .expect("room owner connects")
}

fn bound_jid(client: &WsXmppClient) -> FullJid {
    client
        .full_jid
        .as_deref()
        .expect("bound JID")
        .parse()
        .expect("full JID")
}

fn persistent_database_url(directory: &tempfile::TempDir) -> String {
    format!(
        "sqlite://{}?mode=rwc",
        directory.path().join("characterization.db").display()
    )
}

async fn send_chat(client: &mut WsXmppClient, to: &FullJid, id: &str) {
    let mut message = Message::new(Some(to.clone().into()));
    message.id = Some(Id(id.to_owned()));
    message.type_ = MessageType::Chat;
    message.bodies.insert(Lang::new(), id.to_owned());
    send(client, message.into()).await;
}

async fn receive_chat(client: &mut WsXmppClient, id: &str) -> Element {
    let frame = client
        .recv_matching(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.is("message", JABBER_CLIENT) && element.attr("id") == Some(id)
            })
        })
        .await
        .expect("routed chat message");
    frame.parse().expect("message XML")
}

async fn ping(client: &mut WsXmppClient, id: &str) {
    send(client, Iq::from_get(id, Ping).into()).await;
    let result: Element = client
        .recv_matching(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.is("iq", JABBER_CLIENT) && element.attr("id") == Some(id)
            })
        })
        .await
        .expect("ping result")
        .parse()
        .expect("ping XML");
    assert_eq!(result.attr("type"), Some("result"));
}

// ---------------------------------------------------------------------------
// XEP-0198 helpers
// ---------------------------------------------------------------------------

fn sm_enable_resumption() -> Element {
    Element::builder("enable", SM_NS)
        .attr(minidom::rxml::xml_ncname!("resume").to_owned(), "true")
        .build()
}

fn sm_ack(handled: u32) -> Element {
    Element::builder("a", SM_NS)
        .attr(
            minidom::rxml::xml_ncname!("h").to_owned(),
            handled.to_string(),
        )
        .build()
}

fn sm_request() -> Element {
    Element::builder("r", SM_NS).build()
}

fn sm_resume(stream: &SmSessionId, handled: u32) -> Element {
    Element::builder("resume", SM_NS)
        .attr(
            minidom::rxml::xml_ncname!("previd").to_owned(),
            stream.as_str(),
        )
        .attr(
            minidom::rxml::xml_ncname!("h").to_owned(),
            handled.to_string(),
        )
        .build()
}

async fn enable_resumption(client: &mut WsXmppClient) -> SmSessionId {
    send(client, sm_enable_resumption()).await;
    let enabled = recv_element(client).await;
    assert!(enabled.is("enabled", SM_NS), "enable response: {enabled:?}");
    assert!(matches!(enabled.attr("resume"), Some("true" | "1")));
    SmSessionId::new(enabled.attr("id").expect("resumable stream ID"))
}

async fn send_ack(client: &mut WsXmppClient, handled: u32) {
    send(client, sm_ack(handled)).await;
}

/// `<r/>` → the server's handled-inbound count. The reply is also a barrier:
/// every earlier frame on this stream has been processed.
async fn server_handled(client: &mut WsXmppClient) -> u32 {
    send(client, sm_request()).await;
    let ack = recv_skipping_ack_requests(client).await;
    assert!(ack.is("a", SM_NS), "expected <a/> after <r/>, got {ack:?}");
    ack.attr("h")
        .expect("h attribute")
        .parse()
        .expect("h is a u32")
}

async fn resume(client: &mut WsXmppClient, stream: &SmSessionId, handled: u32) -> Element {
    send(client, sm_resume(stream, handled)).await;
    recv_element(client).await
}

async fn authenticate_without_bind(server: &TestServer, account: (&str, &str)) -> WsXmppClient {
    let mut client = WsXmppClient::connect(&server.ws_url())
        .await
        .expect("replacement transport");
    client
        .authenticate(DOMAIN, account.0, account.1)
        .await
        .expect("authenticate before resume");
    client
}

/// Polls durable storage for the detached snapshot. Transport loss has no
/// wire-visible completion, so the stored row is the barrier; the timeout
/// only detects a detach that never happens.
async fn wait_for_detached(
    storage: &DatabaseSmPersistence,
    stream: &SmSessionId,
) -> PersistedSession {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if let Some(session) = storage.get_session(stream).await.expect("read session") {
                return session;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await
    .expect("transport loss persists the detached session")
}

/// The XEP-0203 `from` of the server's replay stamp, if the frame has one.
fn replay_delay_from(element: &Element) -> Option<&str> {
    element
        .get_child("delay", NS_DELAY)
        .map(|delay| delay.attr("from").expect("delay from"))
}

// ---------------------------------------------------------------------------
// XEP-0045 helpers
// ---------------------------------------------------------------------------

fn join_presence(occupant: &FullJid) -> Element {
    let mut presence = Presence::new(PresenceType::None);
    presence.to = Some(occupant.clone().into());
    presence
        .payloads
        .push(Element::builder("x", NS_MUC).build());
    presence.into()
}

fn groupchat(room: &BareJid, body: &str) -> Element {
    let mut message = Message::new(Some(room.clone().into()));
    message.type_ = MessageType::Groupchat;
    message.id = Some(Id(body.to_owned()));
    message.bodies.insert(Lang::new(), body.to_owned());
    message.into()
}

fn body(element: &Element) -> Option<String> {
    element.get_child("body", JABBER_CLIENT).map(Element::text)
}

fn is_groupchat_with_body(element: &Element, expected: &str) -> bool {
    element.is("message", JABBER_CLIENT)
        && element.attr("type") == Some("groupchat")
        && body(element).as_deref() == Some(expected)
}

fn muc_status_codes(presence: &Element) -> Vec<String> {
    presence
        .get_child("x", NS_MUC_USER)
        .map(|x| {
            x.children()
                .filter(|child| child.is("status", NS_MUC_USER))
                .filter_map(|status| status.attr("code").map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Joins `occupant` and drains the burst through the subject (its last frame).
async fn join(client: &mut WsXmppClient, occupant: &FullJid) {
    send(client, join_presence(occupant)).await;
    client
        .recv_until(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.is("message", JABBER_CLIENT)
                    && element.get_child("subject", JABBER_CLIENT).is_some()
            })
        })
        .await
        .expect("join burst completes");
}

/// XEP-0045 §7.1 order of events, observed by `bob` joining a room in which
/// the owner already sits and has already spoken:
///
/// 1. presence from the existing occupant, 2. self-presence (status 110),
/// 3. no discussion history (§7.2.13 history is a MAY; Waddle sends none, so
///    the owner's earlier message never reaches the joiner), 4. the subject
///    (§7.2.15; the never-set form is an empty `<subject/>` from the room),
/// 5. then live messages — here one the owner sends only after it has seen
///    bob's join presence, i.e. while bob's own burst may still be unwritten.
async fn join_burst_then_live(server: &TestServer) {
    let room: BareJid = "burst-order@muc.localhost".parse().expect("room JID");
    let owner_occupant = room.with_resource_str("owner").expect("owner occupant");
    let bob_occupant = room.with_resource_str("bob").expect("bob occupant");
    let mut owner = connect_owner(server).await;
    join(&mut owner, &owner_occupant).await;
    send(&mut owner, groupchat(&room, "before-join")).await;
    owner
        .recv_matching(|frame| {
            frame
                .parse::<Element>()
                .is_ok_and(|element| is_groupchat_with_body(&element, "before-join"))
        })
        .await
        .expect("the room reflected the pre-join message");

    let mut bob = connect(server, BOB, "phone").await;
    send(&mut bob, join_presence(&bob_occupant)).await;
    // The owner learns of the join inside bob's join handler, before bob's
    // burst is written: sending now races the live message against it.
    owner
        .recv_matching(|frame| {
            frame.parse::<Element>().is_ok_and(|element| {
                element.is("presence", JABBER_CLIENT)
                    && element.attr("from") == Some(bob_occupant.to_string().as_str())
            })
        })
        .await
        .expect("owner observes bob's join presence");
    send(&mut owner, groupchat(&room, "live-after-join")).await;

    let mut frames = Vec::new();
    loop {
        let element = recv_element(&mut bob).await;
        let done = is_groupchat_with_body(&element, "live-after-join");
        frames.push(element);
        if done {
            break;
        }
    }
    let [occupant, self_presence, subject, live] = frames.as_slice() else {
        panic!("expected exactly burst(3) + live(1) frames, got {frames:#?}");
    };
    for (presence, from, is_self) in [
        (occupant, &owner_occupant, false),
        (self_presence, &bob_occupant, true),
    ] {
        assert!(presence.is("presence", JABBER_CLIENT), "{presence:?}");
        assert_eq!(presence.attr("type"), None, "available: {presence:?}");
        assert_eq!(presence.attr("from"), Some(from.to_string().as_str()));
        assert!(
            presence.get_child("x", NS_MUC_USER).is_some(),
            "{presence:?}"
        );
        assert_eq!(
            muc_status_codes(presence)
                .iter()
                .any(|code| code == SELF_PRESENCE_STATUS),
            is_self,
            "status 110 only on self-presence: {presence:?}"
        );
    }
    assert!(subject.is("message", JABBER_CLIENT), "{subject:?}");
    assert_eq!(subject.attr("type"), Some("groupchat"));
    assert_eq!(subject.attr("from"), Some(room.to_string().as_str()));
    let subject_text = subject
        .get_child("subject", JABBER_CLIENT)
        .expect("subject element");
    assert_eq!(subject_text.text(), "", "never-set subject is empty");
    assert_eq!(body(subject), None, "the subject frame carries no history");
    assert_eq!(live.attr("from"), Some(owner_occupant.to_string().as_str()));
    assert_eq!(replay_delay_from(live), None, "live, not history: {live:?}");
    bob.close().await.expect("bob closes");
    owner.close().await.expect("owner closes");
}

#[tokio::test]
async fn muc_join_burst_orders_presence_self_presence_subject_before_live_without_history() {
    let server = TestServer::start_with_extra_accounts(ACCOUNTS);
    join_burst_then_live(&server).await;
}

// ---------------------------------------------------------------------------
// XEP-0198 resume after transport loss
// ---------------------------------------------------------------------------

/// Counters on the wire, XEP-0198 §4–§5: stanzas only (not `<enable/>`,
/// `<r/>`, `<a/>`), outbound starting at the first stanza after `<enabled/>`.
///
/// Outbound: 1 ping result, 2–4 chat m1–m3. Inbound: 1 ping.
/// The client acks 2, loses the transport, then resumes claiming h=3:
/// the server answers `<resumed h='1'/>` and replays only m3, delay-stamped
/// with its original receipt time by the serving domain (#1178). Both counters
/// continue across the resume (ping → inbound 2; outbound 5–6 are accepted).
#[tokio::test]
async fn sm_resume_after_transport_loss_reports_inbound_h_and_replays_unacked_tail() {
    let directory = tempfile::tempdir().expect("database directory");
    let database_url = persistent_database_url(&directory);
    let server = TestServer::start_persistent_with_extra_accounts(&database_url, ACCOUNTS);
    let storage = DatabaseSmPersistence::open(Some(&database_url))
        .await
        .expect("inspection storage");
    let mut bob = connect(&server, BOB, "phone").await;
    let bob_jid = bound_jid(&bob);
    let stream = enable_resumption(&mut bob).await;
    let mut alice = connect(&server, ALICE, "desk").await;

    ping(&mut bob, "p1").await;
    for id in ["m1", "m2", "m3"] {
        send_chat(&mut alice, &bob_jid, id).await;
        let delivered = receive_chat(&mut bob, id).await;
        assert_eq!(replay_delay_from(&delivered), None, "live delivery");
    }
    send_ack(&mut bob, 2).await;
    assert_eq!(server_handled(&mut bob).await, 1, "one inbound stanza (p1)");
    drop(bob);

    let detached = wait_for_detached(&storage, &stream).await;
    assert_eq!(
        (
            detached.inbound_count,
            detached.outbound_count,
            detached.last_acked
        ),
        (1, 4, 2),
        "detached snapshot counters"
    );
    assert_eq!(detached.jid, bob_jid);

    let mut resumed = authenticate_without_bind(&server, BOB).await;
    let response = resume(&mut resumed, &stream, 3).await;
    assert!(
        response.is("resumed", SM_NS),
        "resume response: {response:?}"
    );
    assert_eq!(response.attr("previd"), Some(stream.as_str()));
    assert_eq!(response.attr("h"), Some("1"));
    let replay = recv_skipping_ack_requests(&mut resumed).await;
    assert!(replay.is("message", JABBER_CLIENT), "replay: {replay:?}");
    assert_eq!(
        replay.attr("id"),
        Some("m3"),
        "only the unacknowledged tail"
    );
    assert_eq!(replay_delay_from(&replay), Some(DOMAIN));
    // The next frame is the answer to this request: nothing else was replayed.
    assert_eq!(server_handled(&mut resumed).await, 1);

    ping(&mut resumed, "p2").await;
    assert_eq!(server_handled(&mut resumed).await, 2);
    send_chat(&mut alice, &bob_jid, "m4").await;
    let live = receive_chat(&mut resumed, "m4").await;
    assert_eq!(
        replay_delay_from(&live),
        None,
        "post-resume delivery is live"
    );
    // Outbound continued at 5 (p2 result) and 6 (m4): acking 6 is accepted,
    // and the stream answers the following request.
    send_ack(&mut resumed, 6).await;
    assert_eq!(server_handled(&mut resumed).await, 2);
    resumed.close().await.expect("resumed stream closes");
    alice.close().await.expect("alice closes");
}

// ---------------------------------------------------------------------------
// Detach with an unacknowledged ingress frame
// ---------------------------------------------------------------------------

struct DetachedIngressFrame {
    /// `sm_ingress_appends` rows at the frame's replay slot, read before resume.
    appends: Vec<PersistedIngressAppend>,
    bob_jid: FullJid,
    stream: SmSessionId,
}

/// Bob receives one ingress-routed chat (outbound sequence 1, carrying its
/// ingress append obligation in the live SM queue), never acks it, and loses
/// the transport. Detach authorizes the obligation against canonical ingress
/// state (`drain_append::reserve_live_recorded`) before the snapshot is stored.
/// Resuming with h=0 must still replay the frame.
async fn detach_with_unacked_ingress_frame(
    server: &TestServer,
    database_url: &str,
) -> DetachedIngressFrame {
    let storage = DatabaseSmPersistence::open(Some(database_url))
        .await
        .expect("inspection storage");
    let mut bob = connect(server, BOB, "phone").await;
    let bob_jid = bound_jid(&bob);
    let stream = enable_resumption(&mut bob).await;
    let mut alice = connect(server, ALICE, "desk").await;
    send_chat(&mut alice, &bob_jid, "unacked-ingress").await;
    receive_chat(&mut bob, "unacked-ingress").await;
    drop(bob);
    let detached = wait_for_detached(&storage, &stream).await;
    assert_eq!((detached.outbound_count, detached.last_acked), (1, 0));
    let appends = storage
        .get_ingress_appends_for_sequence(&stream, 1)
        .await
        .expect("read ingress appends");

    let mut resumed = authenticate_without_bind(server, BOB).await;
    let response = resume(&mut resumed, &stream, 0).await;
    assert!(
        response.is("resumed", SM_NS),
        "resume response: {response:?}"
    );
    assert_eq!(response.attr("h"), Some("0"));
    let replay = recv_skipping_ack_requests(&mut resumed).await;
    assert!(replay.is("message", JABBER_CLIENT), "replay: {replay:?}");
    assert_eq!(replay.attr("id"), Some("unacked-ingress"));
    assert_eq!(replay_delay_from(&replay), Some(DOMAIN));
    assert_eq!(
        server_handled(&mut resumed).await,
        0,
        "nothing else replayed"
    );
    resumed.close().await.expect("resumed stream closes");
    alice.close().await.expect("alice closes");
    DetachedIngressFrame {
        appends,
        bob_jid,
        stream,
    }
}

/// With canonical state readable, detach proves the obligation: the detached
/// snapshot is stored together with one pending keyed append for the frame.
#[tokio::test]
async fn sm_detach_keys_unacked_ingress_frame_when_canonical_read_succeeds() {
    let directory = tempfile::tempdir().expect("database directory");
    let database_url = persistent_database_url(&directory);
    let server = TestServer::start_persistent_with_extra_accounts(&database_url, ACCOUNTS);
    let detached = detach_with_unacked_ingress_frame(&server, &database_url).await;
    let [append] = detached.appends.as_slice() else {
        panic!("expected one keyed append, got {:#?}", detached.appends);
    };
    assert_eq!(append.accepting_stream, detached.stream);
    assert_eq!(append.sequence, 1);
    assert_eq!(append.key.resource, detached.bob_jid);
    assert_eq!(append.disposition, IngressCustodyDisposition::Pending);
    assert_eq!(
        append.payload.to_element().attr("id"),
        Some("unacked-ingress")
    );
}

#[cfg(feature = "test-support")]
mod seams {
    //! Deterministic interleavings and faults through the test-support seams.
    //! Every test here needs `--features test-support`.

    use super::*;

    const HOLD_BATCH_KIND_ENV: &str = "WADDLE_TEST_SEAM_HOLD_BATCH_KIND";
    const HOLD_BATCH_ACCOUNT_ENV: &str = "WADDLE_TEST_SEAM_HOLD_BATCH_ACCOUNT";
    const DETACH_CANONICAL_READ_ENV: &str = "WADDLE_TEST_SEAM_DETACH_CANONICAL_READ";
    const BOB_BARE: &str = "bob@localhost";
    /// Failure detector for the concurrent peer; it stops long before this.
    const PEER_MESSAGE_LIMIT: u32 = 2_000;

    /// The live room message is queued on bob's connection while the seam
    /// holds bob's written-inline join burst; bob still sees the full burst
    /// first. Today's ordering comes from the inline handler write
    /// (`connection.rs` inbound arm) preceding the next outbound drain.
    #[tokio::test]
    async fn muc_live_message_queued_during_join_burst_is_written_after_subject() {
        let server = TestServer::start_with_extra_envs(
            ACCOUNTS,
            &[
                (HOLD_BATCH_KIND_ENV, "muc-join"),
                (HOLD_BATCH_ACCOUNT_ENV, BOB_BARE),
            ],
        );
        join_burst_then_live(&server).await;
    }

    fn peer_message_id(sequence: u32) -> String {
        format!("peer-{sequence}")
    }

    /// XEP-0198 §5 replay versus live delivery with a peer delivering to the
    /// resuming full JID before, during and after the resume.
    ///
    /// m1–m3 are written live and left unacknowledged; the transport drops;
    /// the peer then sends continuously (each message paced by a ping round
    /// trip) while bob resumes with h=0. The seam holds `<resumed/>` and its
    /// replay — already registered — until a peer message is queued live on
    /// the resumed connection. Observed: every message arrives exactly once in
    /// send order; the delay-stamped replay (live-written m1–m3 plus whatever
    /// the peer sent while detached) is a contiguous prefix, and the live
    /// frames, of which there is at least one, all follow it.
    #[tokio::test]
    async fn sm_replay_precedes_live_with_concurrently_delivering_peer() {
        let directory = tempfile::tempdir().expect("database directory");
        let database_url = persistent_database_url(&directory);
        let server = TestServer::start_persistent_with_extra_envs(
            &database_url,
            ACCOUNTS,
            &[
                (HOLD_BATCH_KIND_ENV, "sm-resumed"),
                (HOLD_BATCH_ACCOUNT_ENV, BOB_BARE),
            ],
        );
        let storage = DatabaseSmPersistence::open(Some(&database_url))
            .await
            .expect("inspection storage");
        let mut bob = connect(&server, BOB, "phone").await;
        let bob_jid = bound_jid(&bob);
        let stream = enable_resumption(&mut bob).await;
        let mut alice = connect(&server, ALICE, "desk").await;
        for sequence in 1..=3 {
            let id = peer_message_id(sequence);
            send_chat(&mut alice, &bob_jid, &id).await;
            receive_chat(&mut bob, &id).await;
        }
        drop(bob);
        wait_for_detached(&storage, &stream).await;

        let (stop, stopped) = tokio::sync::watch::channel(false);
        let peer_target = bob_jid.clone();
        let peer = tokio::spawn(async move {
            let mut last_sent = 3;
            loop {
                let stop_requested = *stopped.borrow();
                if stop_requested {
                    break;
                }
                assert!(last_sent < PEER_MESSAGE_LIMIT, "peer was never stopped");
                last_sent += 1;
                send_chat(&mut alice, &peer_target, &peer_message_id(last_sent)).await;
                ping(&mut alice, &format!("pace-{last_sent}")).await;
            }
            (alice, last_sent)
        });

        let mut resumed = authenticate_without_bind(&server, BOB).await;
        let response = resume(&mut resumed, &stream, 0).await;
        assert!(
            response.is("resumed", SM_NS),
            "resume response: {response:?}"
        );
        assert_eq!(response.attr("h"), Some("0"));
        stop.send(true).expect("peer is running");
        let (alice, last_sent) = peer.await.expect("peer task");

        let last_id = peer_message_id(last_sent);
        let mut received = Vec::new();
        loop {
            let element = recv_skipping_ack_requests(&mut resumed).await;
            assert!(
                element.is("message", JABBER_CLIENT),
                "only peer messages after <resumed/>: {element:?}"
            );
            let id = element.attr("id").expect("message id").to_owned();
            let replayed = match replay_delay_from(&element) {
                Some(from) => {
                    assert_eq!(from, DOMAIN, "replay stamp");
                    true
                }
                None => false,
            };
            let done = id == last_id;
            received.push((id, replayed));
            if done {
                break;
            }
        }
        let ids: Vec<&str> = received.iter().map(|(id, _)| id.as_str()).collect();
        let expected: Vec<String> = (1..=last_sent).map(peer_message_id).collect();
        assert_eq!(ids, expected, "exactly once, in send order");
        let first_live = received
            .iter()
            .position(|(_, replayed)| !replayed)
            .expect("the seam released on a live message");
        assert!(first_live >= 3, "m1–m3 are replayed: {received:?}");
        assert!(
            received[first_live..].iter().all(|(_, replayed)| !replayed),
            "no replayed frame after the first live one: {received:?}"
        );
        resumed.close().await.expect("resumed stream closes");
        alice.close().await.expect("alice closes");
    }

    /// Characterizes current behaviour; changes with C2h (#2046 contract 4).
    ///
    /// Every canonical read during the detach fails (`CanonicalReadFailed`,
    /// as a database error). `DrainAuthority` degrades the obligation to an
    /// unkeyed entry (decision on #1789: refusing it would turn the outage
    /// into loss): the frame is still persisted and replayed, but no
    /// `sm_ingress_appends` proof is stored for it, so the dedupe key is lost
    /// and only the delivery receipt guards against re-execution. C2h appends
    /// keyed-but-unverified entries at detach with no detach-time canonical
    /// read, so this test then expects the keyed row despite the outage.
    #[tokio::test]
    async fn sm_detach_canonical_read_outage_replays_unacked_ingress_frame_unkeyed() {
        let directory = tempfile::tempdir().expect("database directory");
        let database_url = persistent_database_url(&directory);
        let server = TestServer::start_persistent_with_extra_envs(
            &database_url,
            ACCOUNTS,
            &[(DETACH_CANONICAL_READ_ENV, "unavailable")],
        );
        let detached = detach_with_unacked_ingress_frame(&server, &database_url).await;
        assert_eq!(
            detached.appends,
            Vec::new(),
            "no keyed append survives a canonical-read outage at detach"
        );
    }
}
