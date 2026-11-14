//! The resource model: socket caps, per-peer budgets, deadlines, eviction
//! and its reserve, replacement of a pair's stream, backoff and the
//! shutdown join.
//!
//! Every connection in these tests comes from 127.0.0.1, so a test that
//! opens more than four in a row waits for the per-address accept rate (two
//! a second after a burst of four).

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use waddle_server::s2s::config::ResourceLimits;
use waddle_server::s2s::conn::{ConnectError, StreamDirection};
use waddle_xmpp::s2s::StreamErrorCondition;
use waddle_xmpp::telemetry::attributes::S2sOutcome;
use waddle_xmpp::telemetry::test_support::MetricsTestGuard;

use crate::support::{
    domain, eventually, header, listener, pair, raw_exchange, read_until_closed, start, Deployment,
    Pki, Received, Spec, HOME_A, HOME_B,
};

const WAIT: Duration = Duration::from_secs(5);
const HOME_C: &str = "home-c.test";

/// A pause after which 127.0.0.1 may open one more connection.
const ACCEPT_PAUSE: Duration = Duration::from_millis(600);

/// Wait until `deployment` holds `count` established streams. A dialler's
/// `connect` returns once its own side is established, which can be just
/// before the receiving side registers the stream.
async fn holds_streams(deployment: &Deployment, count: usize) {
    assert!(
        eventually(WAIT, || deployment.handle.streams().len() == count).await,
        "expected {count} streams, found {}",
        deployment.handle.streams().len()
    );
}

/// Inbound connections that ended with `outcome`.
fn inbound_endings(metrics: &MetricsTestGuard, outcome: &str) -> u64 {
    metrics
        .counter_sum(
            "waddle.s2s.connections",
            &[("direction", "inbound"), ("outcome", outcome)],
        )
        .unwrap_or(0)
}

/// Inbound sockets or streams refused by `resource`.
fn inbound_refusals(metrics: &MetricsTestGuard, resource: &str) -> u64 {
    metrics
        .counter_sum(
            "waddle.s2s.capacity.refusals",
            &[("direction", "inbound"), ("resource", resource)],
        )
        .unwrap_or(0)
}

#[tokio::test(flavor = "multi_thread")]
async fn a_socket_over_the_unauthenticated_cap_is_closed_without_xml() {
    let pki = Pki::new();
    let (_a, b) = pair(&pki, |spec| {
        spec.limits = vec!["max_unauthenticated = 1".to_owned()];
    })
    .await;
    // The first socket holds the only unauthenticated slot.
    let _first = TcpStream::connect(b.address).await.expect("connect");
    assert!(eventually(WAIT, || b.handle.open_sockets() == 1).await);
    let received = raw_exchange(b.address, &header(Some(HOME_A), HOME_B), WAIT).await;
    assert!(received.closed_without_xml(), "{received:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn a_silent_peer_is_closed_at_the_stage_deadline() {
    let pki = Pki::new();
    let (_a, b) = pair(&pki, |spec| {
        spec.limits = vec!["stage_timeout_ms = 200".to_owned()];
    })
    .await;
    let started = Instant::now();
    let received = raw_exchange(b.address, b"", WAIT).await;
    assert!(received.closed, "{received:?}");
    assert_eq!(
        received.stream_error(),
        Some(StreamErrorCondition::ConnectionTimeout)
    );
    assert!(received.at.duration_since(started) < Duration::from_secs(3));
}

#[tokio::test(flavor = "multi_thread")]
async fn partial_input_never_extends_a_deadline() {
    let pki = Pki::new();
    // Only the stage deadline is short; the whole-negotiation deadline
    // keeps its default of twenty seconds.
    let (_a, b) = pair(&pki, |spec| {
        spec.limits = vec!["stage_timeout_ms = 300".to_owned()];
    })
    .await;
    let socket = TcpStream::connect(b.address).await.expect("connect");
    let started = Instant::now();
    let (mut reader, mut writer) = socket.into_split();
    // Read concurrently, so the error is collected before writes to the
    // closed connection can reset it, and note when the server closed.
    let reading = tokio::spawn(async move {
        let mut bytes = Vec::new();
        let closed = tokio::time::timeout(WAIT, reader.read_to_end(&mut bytes))
            .await
            .is_ok();
        Received {
            bytes,
            closed,
            at: Instant::now(),
        }
    });
    let initial = header(Some(HOME_A), HOME_B);
    // One byte every 50 ms for two seconds: input keeps arriving long after
    // the stage deadline, and the header is never completed.
    for byte in initial.iter().take(40) {
        if writer.write_all(&[*byte]).await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let received = reading.await.expect("the reader");
    assert!(received.closed, "{received:?}");
    assert_eq!(
        received.stream_error(),
        Some(StreamErrorCondition::ConnectionTimeout),
        "{received:?}"
    );
    // The stream ends at the stage deadline. Had the trickle renewed it,
    // the stream would have lasted at least as long as the trickle.
    let lasted = received.at.duration_since(started);
    assert!(lasted >= Duration::from_millis(250), "{lasted:?}");
    assert!(lasted < Duration::from_millis(1500), "{lasted:?}");
}

#[tokio::test(flavor = "multi_thread")]
async fn shutdown_tells_negotiating_peers_and_joins_every_task() {
    let pki = Pki::new();
    let (a, b) = pair(&pki, |_| {}).await;
    a.handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("an established stream");

    let mut negotiating = TcpStream::connect(b.address).await.expect("connect");
    negotiating
        .write_all(&header(Some(HOME_A), HOME_B))
        .await
        .expect("write");
    assert!(eventually(WAIT, || b.handle.open_sockets() == 2).await);

    let handle = b.handle.clone();
    let report = b.service.shutdown(WAIT).await;
    assert!(report.joined, "{report:?}");
    assert_eq!(handle.open_sockets(), 0);
    assert!(handle.streams().is_empty());
    let received = read_until_closed(&mut negotiating, WAIT).await;
    assert!(received.closed, "{received:?}");
    assert_eq!(
        received.stream_error(),
        Some(StreamErrorCondition::SystemShutdown)
    );

    // The other side sees its stream end.
    assert!(eventually(WAIT, || a.handle.streams().is_empty()).await);
    // A stopped service opens nothing.
    assert_eq!(
        handle.connect(&domain(HOME_B), &domain(HOME_A)).await,
        Err(ConnectError::ShuttingDown)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn unverified_claims_never_use_a_peers_negotiation_budget() {
    let pki = Pki::new();
    let (a, b) = pair(&pki, |_| {}).await;
    let defaults = ResourceLimits::default();
    // Raw sockets claim to be home-a and stop after the plaintext header,
    // as many as home-a's whole per-peer budget.
    let mut claims = Vec::new();
    for _ in 0..defaults.max_negotiating_per_peer {
        let mut socket = TcpStream::connect(b.address).await.expect("connect");
        socket
            .write_all(&header(Some(HOME_A), HOME_B))
            .await
            .expect("write");
        claims.push(socket);
    }
    assert!(eventually(WAIT, || b.handle.open_sockets() == claims.len()).await);
    // The real home-a still reaches home-b, and home-b still reaches it:
    // a claim counts only against the unauthenticated caps until TLS
    // verifies it, and the two directions have separate budgets.
    a.handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("home-a's stream to home-b");
    b.handle
        .connect(&domain(HOME_B), &domain(HOME_A))
        .await
        .expect("home-b's stream to home-a");
}

/// `home-b.test`, admitting both `home-a.test` and `home-c.test`, with
/// `limits`, and those two deployments configured to dial it.
async fn hub(pki: &Pki, limits: Vec<String>) -> (Deployment, Deployment, Deployment) {
    let (listener_b, address_b) = listener().await;
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.allow = vec![HOME_A, HOME_C];
    spec_b.private = vec![HOME_A, HOME_C];
    spec_b.limits = limits;
    let b = start(pki, &spec_b, listener_b).await;
    let spoke = |local: &'static str| {
        let mut spec = Spec::new(local, HOME_B);
        spec.endpoints = vec![(HOME_B, address_b)];
        spec
    };
    let (listener_a, _) = listener().await;
    let a = start(pki, &spoke(HOME_A), listener_a).await;
    let (listener_c, _) = listener().await;
    let c = start(pki, &spoke(HOME_C), listener_c).await;
    (a, b, c)
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_stream_is_evicted_for_a_new_peer_when_the_stream_cap_is_reached() {
    let pki = Pki::new();
    let (a, b, c) = hub(
        &pki,
        vec![
            "max_streams = 1".to_owned(),
            "eviction_idle_ms = 1".to_owned(),
        ],
    )
    .await;
    a.handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("the first stream");
    tokio::time::sleep(Duration::from_millis(50)).await;
    c.handle
        .connect(&domain(HOME_C), &domain(HOME_B))
        .await
        .expect("the second stream evicts the idle first one");
    assert!(eventually(WAIT, || a.handle.streams().is_empty()).await);
    let remaining = b.handle.streams();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].remote(), &domain(HOME_C));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_busy_stream_is_not_evicted_and_the_new_one_is_refused() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let pki = Pki::new();
    let (a, b, c) = hub(&pki, vec!["max_streams = 1".to_owned()]).await;
    a.handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("the first stream");
    holds_streams(&b, 1).await;
    // The default eviction idle time is far longer than this test.
    let refused = c.handle.connect(&domain(HOME_C), &domain(HOME_B)).await;
    assert_eq!(refused, Err(ConnectError::Failed(S2sOutcome::PeerError)));
    // home-b refused it with `resource-constraint` for its stream cap.
    assert!(
        eventually(WAIT, || inbound_refusals(&metrics, "streams") == 1
            && inbound_endings(&metrics, "capacity") == 1)
        .await
    );
    assert_eq!(a.handle.streams().len(), 1);
    assert_eq!(b.handle.streams().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_peer_cap_counts_remote_domains() {
    let pki = Pki::new();
    let (a, b, c) = hub(
        &pki,
        vec![
            "max_peers = 1".to_owned(),
            "eviction_idle_ms = 1".to_owned(),
        ],
    )
    .await;
    a.handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("the first peer");
    tokio::time::sleep(Duration::from_millis(50)).await;
    c.handle
        .connect(&domain(HOME_C), &domain(HOME_B))
        .await
        .expect("the second peer evicts the idle first one");
    assert!(
        eventually(WAIT, || b
            .handle
            .streams()
            .iter()
            .all(|stream| stream.remote() == &domain(HOME_C)))
        .await
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_inbound_socket_cap_with_no_quiet_stream_refuses_at_accept() {
    let pki = Pki::new();
    let (a, b) = pair(&pki, |spec| {
        spec.limits = vec!["max_inbound_sockets = 1".to_owned()];
    })
    .await;
    a.handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("a stream holding the only inbound socket");
    holds_streams(&b, 1).await;
    // The default eviction idle time is far longer than this test, so
    // nothing can make room.
    let received = raw_exchange(b.address, &header(Some(HOME_A), HOME_B), WAIT).await;
    assert!(received.closed_without_xml(), "{received:?}");
    assert_eq!(a.handle.streams().len(), 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_quiet_stream_is_evicted_for_a_new_socket_at_the_default_caps() {
    const LOCALS: [&str; 4] = [
        HOME_B,
        "muc.home-b.test",
        "spaces.home-b.test",
        "pubsub.home-b.test",
    ];
    const SPOKES: [&str; 4] = [
        "spoke-1.test",
        "spoke-2.test",
        "spoke-3.test",
        "spoke-4.test",
    ];
    let pki = Pki::new();
    let defaults = ResourceLimits::default();
    assert_eq!(SPOKES.len() * LOCALS.len(), defaults.max_inbound_sockets);

    let (listener_b, address_b) = listener().await;
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.local = LOCALS.to_vec();
    spec_b.allow = [SPOKES.as_slice(), &[HOME_A]].concat();
    spec_b.private = spec_b.allow.clone();
    // Every cap keeps its default; only the eviction idle time is short.
    spec_b.limits = vec!["eviction_idle_ms = 200".to_owned()];
    let b = start(&pki, &spec_b, listener_b).await;

    let mut spec_spokes = Spec::new(SPOKES[0], HOME_B);
    spec_spokes.local = SPOKES.to_vec();
    spec_spokes.allow = LOCALS.to_vec();
    spec_spokes.private = LOCALS.to_vec();
    spec_spokes.endpoints = LOCALS.iter().map(|local| (*local, address_b)).collect();
    let (listener_spokes, _) = listener().await;
    let spokes = start(&pki, &spec_spokes, listener_spokes).await;

    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.endpoints = vec![(HOME_B, address_b)];
    let (listener_a, _) = listener().await;
    let a = start(&pki, &spec_a, listener_a).await;

    // Sixteen established streams, from four peers to four local domains,
    // fill every inbound socket while the stream and peer caps still have
    // room.
    let mut opened = 0;
    for spoke in SPOKES {
        for local in LOCALS {
            if opened >= 4 {
                tokio::time::sleep(ACCEPT_PAUSE).await;
            }
            spokes
                .handle
                .connect(&domain(spoke), &domain(local))
                .await
                .expect("a stream to home-b");
            opened += 1;
            holds_streams(&b, opened).await;
        }
    }
    assert_eq!(b.handle.open_sockets(), defaults.max_inbound_sockets);
    assert!(b.handle.streams().len() < defaults.max_streams);

    // Once the streams have carried nothing for the eviction time, a new
    // connection takes the socket of one of them.
    tokio::time::sleep(ACCEPT_PAUSE).await;
    a.handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("a stream that evicts a quiet one");
    assert!(
        eventually(WAIT, || spokes.handle.streams().len()
            == defaults.max_inbound_sockets - 1)
        .await
    );
    let inbound = b.handle.streams();
    assert_eq!(inbound.len(), defaults.max_inbound_sockets);
    assert!(inbound
        .iter()
        .any(|stream| stream.remote() == &domain(HOME_A)));
}

#[tokio::test(flavor = "multi_thread")]
async fn open_mode_reserves_a_quarter_of_the_peer_cap_for_allowlisted_peers() {
    const OTHERS: [&str; 4] = ["c.test", "d.test", "e.test", "f.test"];
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let pki = Pki::new();
    let (listener_b, address_b) = listener().await;
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.mode = "open";
    spec_b.allow = vec![HOME_A];
    spec_b.private = [OTHERS.as_slice(), &[HOME_A]].concat();
    // Four peers, one of them reserved; nothing is idle long enough to be
    // evicted.
    spec_b.limits = vec!["max_peers = 4".to_owned()];
    let b = start(&pki, &spec_b, listener_b).await;

    let mut spec_others = Spec::new(OTHERS[0], HOME_B);
    spec_others.local = OTHERS.to_vec();
    spec_others.endpoints = vec![(HOME_B, address_b)];
    let (listener_others, _) = listener().await;
    let others = start(&pki, &spec_others, listener_others).await;
    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.endpoints = vec![(HOME_B, address_b)];
    let (listener_a, _) = listener().await;
    let a = start(&pki, &spec_a, listener_a).await;

    // Three peers that are neither allowlisted nor trusted homes use the
    // general share of the cap.
    for (index, other) in OTHERS[..3].iter().enumerate() {
        others
            .handle
            .connect(&domain(other), &domain(HOME_B))
            .await
            .expect("a peer within the general share");
        holds_streams(&b, index + 1).await;
    }
    // A fourth such peer finds the general share full, although the cap
    // itself has room.
    let refused = others
        .handle
        .connect(&domain(OTHERS[3]), &domain(HOME_B))
        .await;
    assert_eq!(refused, Err(ConnectError::Failed(S2sOutcome::PeerError)));
    assert!(eventually(WAIT, || inbound_refusals(&metrics, "peers") == 1).await);
    // The allowlisted peer takes the reserved place.
    tokio::time::sleep(ACCEPT_PAUSE).await;
    a.handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("the allowlisted peer");
    holds_streams(&b, 4).await;
}

/// Two deployments that both authenticate as `home-a.test`, dialling
/// `home-b.test`.
async fn twins(pki: &Pki, limits: Vec<String>) -> (Deployment, Deployment, Deployment) {
    let (listener_b, address_b) = listener().await;
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.limits = limits;
    let b = start(pki, &spec_b, listener_b).await;
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.endpoints = vec![(HOME_B, address_b)];
    let (first_listener, _) = listener().await;
    let first = start(pki, &spec, first_listener).await;
    let (second_listener, _) = listener().await;
    let second = start(pki, &spec, second_listener).await;
    (first, second, b)
}

/// The connection carrying `deployment`'s only inbound stream, once it has
/// exactly one.
async fn inbound_connection(deployment: &Deployment) -> u64 {
    let only = || {
        let inbound: Vec<u64> = deployment
            .handle
            .streams()
            .into_iter()
            .filter(|stream| stream.direction() == StreamDirection::Inbound)
            .map(|stream| stream.connection())
            .collect();
        match inbound.as_slice() {
            [connection] => Some(*connection),
            _ => None,
        }
    };
    assert!(eventually(WAIT, || only().is_some()).await);
    only().expect("one inbound stream")
}

#[tokio::test(flavor = "multi_thread")]
async fn a_new_stream_for_a_pair_replaces_the_old_one_with_conflict() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let pki = Pki::new();
    let (first, second, b) = twins(&pki, vec!["replacement_interval_ms = 1".to_owned()]).await;
    first
        .handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("the first stream");
    let old = inbound_connection(&b).await;
    tokio::time::sleep(Duration::from_millis(50)).await;
    let new = second
        .handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("the replacing stream");
    // home-b closed the replaced stream with `conflict`, and its peer saw
    // it end.
    assert!(eventually(WAIT, || inbound_endings(&metrics, "conflict") == 1).await);
    assert!(eventually(WAIT, || first.handle.streams().is_empty()).await);
    // The new stream carries the pair on both sides.
    assert_ne!(inbound_connection(&b).await, old);
    assert_eq!(second.handle.streams(), vec![new]);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_younger_than_the_replacement_interval_is_kept() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let pki = Pki::new();
    let (first, second, b) = twins(&pki, Vec::new()).await;
    first
        .handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("the first stream");
    let kept = inbound_connection(&b).await;
    let refused = second
        .handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await;
    assert_eq!(refused, Err(ConnectError::Failed(S2sOutcome::PeerError)));
    // home-b refused the newcomer with `conflict`.
    assert!(eventually(WAIT, || inbound_endings(&metrics, "conflict") == 1).await);
    assert_eq!(first.handle.streams().len(), 1);
    assert_eq!(inbound_connection(&b).await, kept);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_full_backoff_table_never_refuses_a_healthy_destination() {
    let pki = Pki::new();
    // Ports that were bound and released, so nothing listens on them.
    let (unused_one, dead_one) = listener().await;
    let (unused_two, dead_two) = listener().await;
    drop(unused_one);
    drop(unused_two);
    let (listener_b, address_b) = listener().await;
    let _b = start(&pki, &Spec::new(HOME_B, HOME_A), listener_b).await;
    let mut spec = Spec::new(HOME_A, HOME_B);
    spec.allow = vec![HOME_B, "dead-1.test", "dead-2.test"];
    spec.endpoints = vec![
        (HOME_B, address_b),
        ("dead-1.test", dead_one),
        ("dead-2.test", dead_two),
    ];
    spec.limits = vec![
        "max_backoff_destinations = 1".to_owned(),
        "backoff_initial_ms = 60000".to_owned(),
    ];
    let (listener_a, _) = listener().await;
    let a = start(&pki, &spec, listener_a).await;
    let home_a = domain(HOME_A);

    assert_eq!(
        a.handle.connect(&home_a, &domain("dead-1.test")).await,
        Err(ConnectError::Failed(S2sOutcome::Io))
    );
    assert_eq!(
        a.handle.connect(&home_a, &domain("dead-1.test")).await,
        Err(ConnectError::BackingOff)
    );
    // The table is full, yet a domain without an entry is dialled.
    a.handle
        .connect(&home_a, &domain(HOME_B))
        .await
        .expect("a healthy destination");
    // A second failing domain takes the only entry, so the first one is
    // dialled again rather than refused.
    assert_eq!(
        a.handle.connect(&home_a, &domain("dead-2.test")).await,
        Err(ConnectError::Failed(S2sOutcome::Io))
    );
    assert_eq!(
        a.handle.connect(&home_a, &domain("dead-2.test")).await,
        Err(ConnectError::BackingOff)
    );
    assert_eq!(
        a.handle.connect(&home_a, &domain("dead-1.test")).await,
        Err(ConnectError::Failed(S2sOutcome::Io))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn attempts_for_different_pairs_to_one_domain_run_together() {
    let pki = Pki::new();
    let (listener_b, address_b) = listener().await;
    let mut spec_b = Spec::new(HOME_B, HOME_A);
    spec_b.allow = vec![HOME_A, "muc.home-a.test"];
    spec_b.private = spec_b.allow.clone();
    let _b = start(&pki, &spec_b, listener_b).await;
    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.local = vec![HOME_A, "muc.home-a.test"];
    spec_a.endpoints = vec![(HOME_B, address_b)];
    let (listener_a, _) = listener().await;
    let a = start(&pki, &spec_a, listener_a).await;
    let (home_a, muc_home_a, home_b) = (domain(HOME_A), domain("muc.home-a.test"), domain(HOME_B));
    let (account, muc) = tokio::join!(
        a.handle.connect(&home_a, &home_b),
        a.handle.connect(&muc_home_a, &home_b),
    );
    assert!(account.is_ok(), "{account:?}");
    assert!(muc.is_ok(), "{muc:?}");
    assert_eq!(a.handle.streams().len(), 2);
}
