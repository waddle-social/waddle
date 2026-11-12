use std::collections::HashSet;
use std::time::Duration;

use jid::{FullJid, Jid};
use minidom::Element;
use tokio::net::UdpSocket;
use tokio::time::{timeout, Instant};
use waddle_server::test_support::iq_forward::{
    IqForwardObservation, IqForwardObserverConfig, IqForwardPhase, IQ_FORWARD_OBSERVER_ENV,
};
use waddle_ws_test_support::{TestServer, WsXmppClient};
use waddle_xmpp::stream_management::SM_NS;
use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType};

use super::{
    assert_forwarded_ping, assert_held, assert_iq_error, connect, enable_resumable_sm,
    expect_marker, next_element, next_stanza, ping, send, send_marker, sm_ack, ACCOUNTS, ALICE,
    BOB, HOLD_WINDOW, OUTBOUND_CAPACITY, RECV, SEND_WINDOW_HIGH,
};

const WEDGE_TIMEOUT: Duration = Duration::from_secs(15);
const WEDGE_BUDGET: Duration = Duration::from_secs(45);
/// The handler cannot start before the client sends. Requiring its Pending
/// observation within this allowance therefore bounds all time from backstop
/// start through blocklist lookup to the first observed Pending poll.
const ENTRY_ALLOWANCE: Duration = Duration::from_secs(1);
const ALICE_WAIT: &str = "wait-cycle-alice";
const BOB_WAIT: &str = "wait-cycle-bob";

#[derive(Debug, Clone, Copy)]
enum EntryOrder {
    Simultaneous,
    Staggered,
}

fn fill_id(owner: &str, index: usize) -> String {
    format!("{owner}-{index}")
}

fn sm_request() -> Element {
    Element::builder("r", SM_NS).build()
}

async fn next_non_request(client: &mut WsXmppClient, budget: Duration) -> Element {
    let deadline = Instant::now() + budget;
    loop {
        let element = next_element(client, deadline.saturating_duration_since(Instant::now()))
            .await
            .expect("cycle frame before deadline");
        if !element.is("r", SM_NS) {
            return element;
        }
    }
}

async fn expect_handled(client: &mut WsXmppClient, handled: usize, who: &str) {
    let element = next_non_request(client, RECV).await;
    assert!(
        element.is("a", SM_NS),
        "{who}: expected <a/>, got {element:?}"
    );
    assert_eq!(
        element.attr("h").and_then(|h| h.parse::<usize>().ok()),
        Some(handled),
        "{who}: completed inbound stanza count"
    );
}

async fn observation(socket: &UdpSocket, budget: Duration) -> (IqForwardObservation, Instant) {
    let mut bytes = [0; 1024];
    let length = timeout(budget, socket.recv(&mut bytes))
        .await
        .expect("IQ forward observation before deadline")
        .expect("receive IQ forward observation");
    let at = Instant::now();
    let event = serde_json::from_slice(&bytes[..length]).expect("typed IQ forward observation");
    eprintln!("IQ forward observation at {at:?}: {event:?}");
    (event, at)
}

async fn expect_pending(socket: &UdpSocket, expected: &[(&str, Instant)]) {
    let mut pending = HashSet::new();
    for _ in expected {
        let (event, received_at) = observation(socket, RECV).await;
        assert_eq!(
            event.phase,
            IqForwardPhase::Pending,
            "each handler must enter before either exits"
        );
        let (_, sent_at) = expected
            .iter()
            .find(|(id, _)| *id == event.request_id)
            .unwrap_or_else(|| panic!("unexpected request: {event:?}"));
        assert!(
            received_at - *sent_at <= ENTRY_ALLOWANCE,
            "{event:?}: Pending observation must bound handler entry within {ENTRY_ALLOWANCE:?}"
        );
        assert!(
            pending.insert(event.request_id),
            "duplicate pending observation"
        );
    }
}

async fn no_observation(socket: &UdpSocket) {
    let mut bytes = [0; 1024];
    match timeout(HOLD_WINDOW, socket.recv(&mut bytes)).await {
        Err(_) => {}
        Ok(result) => {
            panic!("observed send futures must remain pending: {result:?}")
        }
    }
}

async fn drain_recovered_stream(
    client: &mut WsXmppClient,
    own: &FullJid,
    peer: &FullJid,
    own_wait: &str,
    peer_wait: &str,
    own_outcome: IqForwardPhase,
    peer_outcome: IqForwardPhase,
) {
    if own_outcome == IqForwardPhase::Cancelled {
        let frame = next_stanza(client, own_wait).await;
        assert_iq_error(
            frame,
            own_wait,
            Jid::from(peer.clone()),
            None,
            ErrorType::Wait,
            DefinedCondition::ResourceConstraint,
            own_wait,
        );
    }
    // The ACK request was sent behind the blocked IQ and the recovery ACK.
    // The count proves that this handler finished, independently of its peer.
    expect_handled(client, SEND_WINDOW_HIGH + OUTBOUND_CAPACITY + 1, own_wait).await;
    let peer_name = if peer_wait == ALICE_WAIT {
        "alice"
    } else {
        "bob"
    };
    for index in SEND_WINDOW_HIGH..SEND_WINDOW_HIGH + OUTBOUND_CAPACITY {
        let frame = next_stanza(client, own_wait).await;
        assert_forwarded_ping(frame, &fill_id(peer_name, index), peer, own, own_wait);
    }
    if peer_outcome == IqForwardPhase::Completed {
        let frame = next_stanza(client, own_wait).await;
        assert_forwarded_ping(frame, peer_wait, peer, own, own_wait);
    }
}

/// Both local IQ send futures actually poll Pending against completed full
/// outbound channels. Recovery ACKs are sent while both are active, and neither
/// future nor either stream advances during the bounded observation window.
/// At least one real 15 s handler backstop is needed to break the cycle.
/// Pending receipt within the 1 s entry allowance bounds work before Pending; each
/// cancellation must report at least 14 s pending on the server's own clock.
/// Observer receive delay cannot make an early cancellation meet that bound.
/// Completion duration is captured before the poll that returns Ready and must
/// meet the bound adjusted for the actual entry stagger. Report ordering and a
/// shared server clock both require cancellation before any normal completion,
/// including when UDP delivery reorders or reporting is preempted.
///
/// The peer can then complete normally once capacity drains, or reach its own
/// backstop first. Observe and validate each outcome separately: cancellation
/// owes resource-constraint/wait; completion delivers the IQ to its peer. Both
/// count as handled, and all queued fills retain FIFO order. No mandatory
/// two-timeout claim is made. The test-support observer reports actual future
/// polls and drops without holding a handler or changing a production deadline.
///
/// Characterizes current behaviour; changes with D2d (#2046), which removes
/// this IQ capacity wait. D2p records the class-wide exit criterion.
async fn observe_cycle(entry: EntryOrder) {
    let observer = UdpSocket::bind("127.0.0.1:0").await.expect("bind observer");
    let config = serde_json::to_string(&IqForwardObserverConfig {
        destination: observer.local_addr().expect("observer address"),
        request_ids: [ALICE_WAIT.to_owned(), BOB_WAIT.to_owned()],
    })
    .expect("serialize observer configuration");
    let server =
        TestServer::start_with_extra_envs(&ACCOUNTS, &[(IQ_FORWARD_OBSERVER_ENV, config.as_str())]);
    let (mut alice, alice_jid) = connect(&server, ALICE, "desk").await;
    let (mut bob, bob_jid) = connect(&server, BOB, "web").await;
    enable_resumable_sm(&mut alice, "alice").await;
    enable_resumable_sm(&mut bob, "bob").await;

    for index in 0..SEND_WINDOW_HIGH {
        send(&mut alice, ping(bob_jid.clone(), &fill_id("alice", index))).await;
    }
    for index in 0..SEND_WINDOW_HIGH {
        let frame = next_stanza(&mut bob, "bob").await;
        assert_forwarded_ping(frame, &fill_id("alice", index), &alice_jid, &bob_jid, "bob");
    }
    for index in 0..SEND_WINDOW_HIGH {
        send(&mut bob, ping(alice_jid.clone(), &fill_id("bob", index))).await;
    }
    for index in 0..SEND_WINDOW_HIGH {
        let frame = next_stanza(&mut alice, "alice").await;
        assert_forwarded_ping(frame, &fill_id("bob", index), &bob_jid, &alice_jid, "alice");
    }

    let filled = SEND_WINDOW_HIGH + OUTBOUND_CAPACITY;
    for index in SEND_WINDOW_HIGH..filled {
        send(&mut alice, ping(bob_jid.clone(), &fill_id("alice", index))).await;
        send(&mut bob, ping(alice_jid.clone(), &fill_id("bob", index))).await;
    }
    // SM controls bypass send-window pacing. Each <a h='1056'/> proves every
    // preceding fill on that stream completed, unlike a socket send alone.
    send(&mut alice, sm_request()).await;
    send(&mut bob, sm_request()).await;
    tokio::join!(
        expect_handled(&mut alice, filled, "alice fill barrier"),
        expect_handled(&mut bob, filled, "bob fill barrier"),
    );

    let alice_started = Instant::now();
    send(&mut alice, ping(bob_jid.clone(), ALICE_WAIT)).await;
    if matches!(entry, EntryOrder::Staggered) {
        expect_pending(&observer, &[(ALICE_WAIT, alice_started)]).await;
        // Observe the first active wait for the existing absence window before
        // starting its peer. This leaves different real backstop deadlines.
        tokio::join!(
            assert_held(&mut alice, "alice before peer enters"),
            no_observation(&observer),
        );
    }
    let bob_started = Instant::now();
    send(&mut bob, ping(alice_jid.clone(), BOB_WAIT)).await;
    match entry {
        EntryOrder::Simultaneous => {
            expect_pending(
                &observer,
                &[(ALICE_WAIT, alice_started), (BOB_WAIT, bob_started)],
            )
            .await;
        }
        EntryOrder::Staggered => expect_pending(&observer, &[(BOB_WAIT, bob_started)]).await,
    }

    // Neither handler can have reached its 15 s backstop yet. ACK both while
    // both capacity waits are active; the observer continues to run alongside
    // concurrent, independent wire observations of both streams.
    let recovery_at = Instant::now();
    let earliest_started = alice_started.min(bob_started);
    assert!(recovery_at - earliest_started < WEDGE_TIMEOUT - HOLD_WINDOW);
    let (alice_ack_at, bob_ack_at) = tokio::join!(
        async {
            send(&mut alice, sm_ack(SEND_WINDOW_HIGH)).await;
            let sent_at = Instant::now();
            send(&mut alice, sm_request()).await;
            sent_at
        },
        async {
            send(&mut bob, sm_ack(SEND_WINDOW_HIGH)).await;
            let sent_at = Instant::now();
            send(&mut bob, sm_request()).await;
            sent_at
        },
    );
    eprintln!(
        "alice recovery ACK sent at {alice_ack_at:?}, {:?} after IQ send",
        alice_ack_at - alice_started
    );
    eprintln!(
        "bob recovery ACK sent at {bob_ack_at:?}, {:?} after IQ send",
        bob_ack_at - bob_started
    );
    assert!(
        alice_ack_at.max(bob_ack_at) - earliest_started < WEDGE_TIMEOUT - HOLD_WINDOW,
        "both recovery ACKs must precede the earliest possible backstop by the hold window"
    );
    tokio::join!(
        assert_held(&mut alice, "alice after recovery ACK"),
        assert_held(&mut bob, "bob after recovery ACK"),
        no_observation(&observer),
    );

    let deadline = recovery_at + WEDGE_BUDGET;
    let mut outcomes = [None; 2];
    let mut terminals = Vec::new();
    for _ in 0..2 {
        let (event, _) = observation(
            &observer,
            deadline.saturating_duration_since(Instant::now()),
        )
        .await;
        let index = match event.request_id.as_str() {
            ALICE_WAIT => 0,
            BOB_WAIT => 1,
            other => panic!("unexpected observed request {other}"),
        };
        assert!(outcomes[index].is_none(), "duplicate terminal observation");
        assert_ne!(event.phase, IqForwardPhase::Pending);
        if event.phase == IqForwardPhase::Cancelled {
            assert!(
                event.pending_for >= WEDGE_TIMEOUT - ENTRY_ALLOWANCE,
                "{event:?}: at least 14 s of event-local pending time is required"
            );
        } else {
            let started = [alice_started, bob_started][index];
            let stagger = started - earliest_started;
            assert!(
                event.pending_for >= WEDGE_TIMEOUT.saturating_sub(ENTRY_ALLOWANCE + stagger),
                "{event:?}: completion predates the backstop bound adjusted for {stagger:?}"
            );
        }
        outcomes[index] = Some(event.phase);
        terminals.push(event);
    }
    terminals.sort_unstable_by_key(|event| event.sequence);
    assert_ne!(
        terminals[0].sequence, terminals[1].sequence,
        "distinct terminal events"
    );
    assert_eq!(
        terminals[0].phase,
        IqForwardPhase::Cancelled,
        "a normal send completion must not precede the first backstop"
    );
    let first_cancelled_at = terminals[0].since_epoch;
    for event in &terminals {
        if event.phase == IqForwardPhase::Completed {
            assert!(
                event.since_epoch >= first_cancelled_at,
                "{event:?}: the Ready poll must start after the first cancellation"
            );
        }
    }
    eprintln!("{entry:?} cycle outcomes: {outcomes:?}");
    let [Some(alice_outcome), Some(bob_outcome)] = outcomes else {
        panic!("both handler outcomes must be observed: {outcomes:?}");
    };
    assert!(
        outcomes.contains(&Some(IqForwardPhase::Cancelled)),
        "a backstop must break the cycle"
    );
    tokio::join!(
        drain_recovered_stream(
            &mut alice,
            &alice_jid,
            &bob_jid,
            ALICE_WAIT,
            BOB_WAIT,
            alice_outcome,
            bob_outcome,
        ),
        drain_recovered_stream(
            &mut bob,
            &bob_jid,
            &alice_jid,
            BOB_WAIT,
            ALICE_WAIT,
            bob_outcome,
            alice_outcome,
        ),
    );
    // Check recipient queues too: a cancelled IQ cannot hide after the fills,
    // and a normally completed IQ must appear exactly once before the marker.
    send_marker(&mut alice, &bob_jid, "alice-marker").await;
    send_marker(&mut bob, &alice_jid, "bob-marker").await;
    tokio::join!(
        expect_marker(&mut bob, "alice-marker", &alice_jid, &bob_jid, "bob"),
        expect_marker(&mut alice, "bob-marker", &bob_jid, &alice_jid, "alice"),
    );
}

/// Two independently observed active sends with near-simultaneous entry.
#[tokio::test]
async fn mutual_full_outbound_channels_hold_iq_forwarders_despite_recovery_acks_until_a_backstop() {
    observe_cycle(EntryOrder::Simultaneous).await;
}

/// The entry offset exposes the peer-completion path after the first backstop
/// on local runs. Validate every observed cancellation or completion without
/// assuming that a particular timer must win a scheduler race on every host.
#[tokio::test]
async fn staggered_full_outbound_channels_hold_both_iq_forwarders_despite_recovery_acks() {
    observe_cycle(EntryOrder::Staggered).await;
}
