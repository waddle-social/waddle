//! The resource model: socket caps, deadlines, eviction, replacement of a
//! pair's stream and the shutdown join.

use std::time::{Duration, Instant};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use waddle_server::s2s::conn::{ConnectError, StreamDirection};
use waddle_xmpp::telemetry::attributes::S2sOutcome;

use crate::support::{
    domain, eventually, has_stream_error, header, listener, pair, raw_exchange, read_until_closed,
    start, Deployment, Pki, Spec, HOME_A, HOME_B,
};

const WAIT: Duration = Duration::from_secs(5);
const HOME_C: &str = "home-c.test";

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
    assert!(received.is_empty(), "{received}");
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
    assert!(
        has_stream_error(&received, "connection-timeout"),
        "{received}"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[tokio::test(flavor = "multi_thread")]
async fn partial_input_never_extends_a_deadline() {
    let pki = Pki::new();
    let (_a, b) = pair(&pki, |spec| {
        spec.limits = vec![
            "stage_timeout_ms = 300".to_owned(),
            "negotiation_timeout_ms = 600".to_owned(),
        ];
    })
    .await;
    let socket = TcpStream::connect(b.address).await.expect("connect");
    let (mut reader, mut writer) = socket.into_split();
    let started = Instant::now();
    // Read concurrently, so the error is collected before writes to the
    // closed connection can reset it.
    let received = tokio::spawn(async move {
        let mut received = Vec::new();
        let _ = tokio::time::timeout(WAIT, reader.read_to_end(&mut received)).await;
        String::from_utf8_lossy(&received).into_owned()
    });
    let initial = header(Some(HOME_A), HOME_B);
    // One byte every 50 ms: the header would take far longer than the
    // stage deadline.
    for byte in initial.iter().take(40) {
        if writer.write_all(&[*byte]).await.is_err() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let received = received.await.expect("the reader");
    assert!(
        has_stream_error(&received, "connection-timeout"),
        "{received}"
    );
    assert!(started.elapsed() < Duration::from_secs(3));
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
    assert!(has_stream_error(&received, "system-shutdown"), "{received}");

    // The other side sees its stream end.
    assert!(eventually(WAIT, || a.handle.streams().is_empty()).await);
    // A stopped service opens nothing.
    assert_eq!(
        handle.connect(&domain(HOME_B), &domain(HOME_A)).await,
        Err(ConnectError::ShuttingDown)
    );
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
    let pki = Pki::new();
    let (a, b, c) = hub(&pki, vec!["max_streams = 1".to_owned()]).await;
    a.handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("the first stream");
    // The default eviction idle time is far longer than this test.
    let refused = c.handle.connect(&domain(HOME_C), &domain(HOME_B)).await;
    assert!(
        matches!(refused, Err(ConnectError::Failed(S2sOutcome::PeerError))),
        "{refused:?}"
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

#[tokio::test(flavor = "multi_thread")]
async fn a_new_stream_for_a_pair_replaces_the_old_one_with_conflict() {
    let pki = Pki::new();
    let (first, second, b) = twins(&pki, vec!["replacement_interval_ms = 1".to_owned()]).await;
    let old = first
        .handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("the first stream");
    tokio::time::sleep(Duration::from_millis(50)).await;
    let new = second
        .handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("the replacing stream");
    assert!(eventually(WAIT, || first.handle.streams().is_empty()).await);
    let inbound: Vec<_> = b
        .handle
        .streams()
        .into_iter()
        .filter(|stream| stream.direction() == StreamDirection::Inbound)
        .collect();
    assert_eq!(inbound.len(), 1);
    assert_ne!(old.certificate_sha256(), new.certificate_sha256());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_stream_younger_than_the_replacement_interval_is_kept() {
    let pki = Pki::new();
    let (first, second, _b) = twins(&pki, Vec::new()).await;
    first
        .handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("the first stream");
    let refused = second
        .handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await;
    assert_eq!(refused, Err(ConnectError::Failed(S2sOutcome::PeerError)));
    assert_eq!(first.handle.streams().len(), 1);
}
