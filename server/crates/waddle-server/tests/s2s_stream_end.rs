//! How an established S2S stream's end is reported to the stanza sink
//! (`StanzaSink::stream_ended`): once per stream, as `StreamEnd::Closed`
//! for an orderly end and `StreamEnd::Lost` when stanzas in flight may have
//! been lost.
//!
//! - Inbound, from a hand-written peer: the peer's closing stream tag, its
//!   `connection-timeout` stream error, the connection's end of file
//!   between elements and the idle timeout with nothing unfinished are
//!   orderly, as peers end idle streams this way; the connection's end of
//!   file, the idle timeout or the maximum age while the peer is in the
//!   middle of an element, and a first-level element left unfinished past
//!   the incomplete-element deadline, are lost.
//! - Inbound, with a sink that holds a stanza until the test releases it:
//!   the maximum age passing while the stanza waits ends the stream before
//!   the stanzas the peer wrote meanwhile are read, which is lost; a wait
//!   longer than the idle timeout does not end the stream, and the stanzas
//!   written meanwhile are read once the wait ends.
//! - Outbound, through a TCP relay that stops reading from the dialling
//!   deployment once the stream is established: a write that does not
//!   complete within the write deadline is lost.
//!
//! Every connection is real loopback TCP, every certificate comes from a
//! test CA and the certificate clock is fixed, as in the connection-manager
//! suite whose fixture this suite shares. No PostgreSQL, SpiceDB or second
//! process is involved.

// The support module is shared with the connection-manager, XEP-0368 and
// XEP-0220 suites; this suite uses only part of it.
#[allow(dead_code)]
#[path = "s2s_connection_manager/support.rs"]
mod support;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use jid::Jid;
use minidom::Element;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{TcpSocket, TcpStream};
use tokio::sync::Semaphore;
use tokio_util::sync::CancellationToken;
use waddle_server::s2s::conn::{
    ServiceOptions, SinkVerdict, SinkWait, StanzaSink, StreamDirection, StreamEnd, TransmitError,
    VerifiedPeerStream, Waited,
};
use waddle_xmpp::s2s::{
    into_server_content, HeaderAddress, StreamEncoder, StreamErrorCondition, StreamHeader,
    StreamVersion,
};
use xmpp_parsers::message::{Id, Lang, Message, MessageType};

use support::{
    domain, eventually, listener, start, start_with, Deployment, Pki, RawPeer, Spec, HOME_A, HOME_B,
};

const WAIT: Duration = Duration::from_secs(5);

/// A short deadline for the stream behaviour a test exercises, so the test
/// does not wait for the default.
const SHORT_DEADLINE_MS: u64 = 300;

/// A short idle timeout that still leaves time to see the stream registered
/// before it ends.
const SHORT_IDLE_MS: u64 = 1500;

/// The body of each stanza written to a stalled relay.
const BULK_BODY_BYTES: usize = 16 * 1024;

/// How many stanzas a test writes towards a stalled relay at most: far more
/// than loopback socket buffers hold.
const MAX_BULK_STANZAS: usize = 4096;

/// The receive buffer of the relay's accepted socket, kept small so a
/// stalled relay fills quickly.
const RELAY_RECEIVE_BUFFER: u32 = 4096;

/// One `stream_ended` call: the stream's direction, its remote domain and
/// how it ended.
type Ending = (StreamDirection, String, StreamEnd);

/// A sink that refuses every stanza and keeps every stream end it is told
/// about.
#[derive(Debug, Default)]
struct EndingSink {
    endings: Mutex<Vec<Ending>>,
}

impl EndingSink {
    fn endings(&self) -> Vec<Ending> {
        match self.endings.lock() {
            Ok(endings) => endings.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl StanzaSink for EndingSink {
    fn offer(&self, _stream: &VerifiedPeerStream, _stanza: Element) -> SinkVerdict {
        SinkVerdict::Refuse
    }

    fn stream_ended(&self, stream: &VerifiedPeerStream, end: StreamEnd) {
        let ending = (stream.direction(), stream.remote().as_str().to_owned(), end);
        match self.endings.lock() {
            Ok(mut endings) => endings.push(ending),
            Err(poisoned) => poisoned.into_inner().push(ending),
        }
    }
}

/// Wait until `sink` has been told about a stream end and return every end
/// it was told about.
async fn endings(sink: &EndingSink) -> Vec<Ending> {
    assert!(
        eventually(WAIT, || !sink.endings().is_empty()).await,
        "the sink was not told that the stream ended"
    );
    sink.endings()
}

/// The id of the stanza [`HoldingSink`] holds.
const HELD: &str = "held";

/// A sink that takes every stanza at once except the one whose id is
/// [`HELD`], which it takes only once the test releases it, as a router
/// does with a stanza that waits for a place on its queues. It keeps the
/// ids it was offered and every stream end it is told about.
#[derive(Debug)]
struct HoldingSink {
    release: Arc<Semaphore>,
    offered: Mutex<Vec<String>>,
    endings: EndingSink,
}

impl HoldingSink {
    fn new() -> Self {
        Self {
            release: Arc::new(Semaphore::new(0)),
            offered: Mutex::new(Vec::new()),
            endings: EndingSink::default(),
        }
    }

    /// Take the held stanza.
    fn release(&self) {
        self.release.add_permits(1);
    }

    fn offered(&self) -> Vec<String> {
        match self.offered.lock() {
            Ok(offered) => offered.clone(),
            Err(poisoned) => poisoned.into_inner().clone(),
        }
    }
}

impl StanzaSink for HoldingSink {
    fn offer(&self, _stream: &VerifiedPeerStream, stanza: Element) -> SinkVerdict {
        let id = stanza.attr("id").unwrap_or_default().to_owned();
        match self.offered.lock() {
            Ok(mut offered) => offered.push(id.clone()),
            Err(poisoned) => poisoned.into_inner().push(id.clone()),
        }
        if id != HELD {
            return SinkVerdict::Accepted;
        }
        let release = Arc::clone(&self.release);
        SinkVerdict::Wait(SinkWait::new(async move {
            match release.acquire_owned().await {
                Ok(_permit) => Waited::Taken,
                Err(_) => Waited::Overloaded,
            }
        }))
    }

    fn stream_ended(&self, stream: &VerifiedPeerStream, end: StreamEnd) {
        self.endings.stream_ended(stream, end);
    }
}

/// Wait until `deployment` holds `count` established streams.
async fn holds_streams(deployment: &Deployment, count: usize) {
    assert!(
        eventually(WAIT, || deployment.handle.streams().len() == count).await,
        "expected {count} streams, found {}",
        deployment.handle.streams().len()
    );
}

/// `home-b.test` with `limits` and a recording sink, and a hand-written
/// peer for `home-a.test` with an established inbound stream to it.
async fn inbound(pki: &Pki, limits: &[&str]) -> (Deployment, Arc<EndingSink>, RawPeer) {
    let sink = Arc::new(EndingSink::default());
    let (b, peer) = inbound_to(pki, limits, sink.clone()).await;
    (b, sink, peer)
}

/// `home-b.test` with `limits` and `sink`, and a hand-written peer for
/// `home-a.test` with an established inbound stream to it.
async fn inbound_to(
    pki: &Pki,
    limits: &[&str],
    sink: Arc<dyn StanzaSink>,
) -> (Deployment, RawPeer) {
    let (listener_b, address_b) = listener().await;
    let mut spec = Spec::new(HOME_B, HOME_A);
    spec.limits = limits.iter().map(|line| (*line).to_owned()).collect();
    let options = ServiceOptions {
        sink,
        ..ServiceOptions::default()
    };
    let b = start_with(pki, &spec, listener_b, options).await;
    let peer = RawPeer::establish(pki, address_b, HOME_A, HOME_B).await;
    holds_streams(&b, 1).await;
    (b, peer)
}

fn jid(raw: &str) -> Jid {
    raw.parse().expect("a test JID")
}

/// A chat message from home-a to home-b with a body of `body_bytes`.
fn message(id: String, body_bytes: usize) -> Element {
    let mut message = Message::new_with_type(MessageType::Chat, Some(jid("bob@home-b.test")));
    message.from = Some(jid("alice@home-a.test/phone"));
    message.id = Some(Id(id));
    Element::from(message.with_body(Lang::default(), "x".repeat(body_bytes)))
}

/// The closing stream tag of a stream from home-a to home-b, as the codec
/// writes it.
fn closing_tag() -> Vec<u8> {
    let header = StreamHeader {
        from: HeaderAddress::Domain(domain(HOME_A)),
        to: HeaderAddress::Domain(domain(HOME_B)),
        id: None,
        version: Some(StreamVersion::V1_0),
        lang: None,
        declare_dialback: false,
    };
    let (mut encoder, _opening) = StreamEncoder::open(&header).expect("a stream header");
    encoder.close().expect("a closing stream tag")
}

#[tokio::test(flavor = "multi_thread")]
async fn end_of_file_between_elements_is_closed() {
    let pki = Pki::new();
    let (b, sink, mut peer) = inbound(&pki, &[]).await;
    // Read what the deployment sent first, so the socket closes with a FIN
    // rather than a reset for unread input.
    let (error, closed, _) = peer.read_until_closed(Duration::from_millis(100)).await;
    assert!(!closed, "the deployment closed the stream");
    assert_eq!(error, None);
    // The peer's socket closes without a TLS close_notify and without a
    // closing stream tag, as a peer drops an idle connection.
    drop(peer);
    assert_eq!(
        endings(&sink).await,
        vec![(
            StreamDirection::Inbound,
            HOME_A.to_owned(),
            StreamEnd::Closed
        )]
    );
    assert!(eventually(WAIT, || b.handle.streams().is_empty()).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn end_of_file_in_the_middle_of_an_element_is_lost() {
    let pki = Pki::new();
    let (b, sink, mut peer) = inbound(&pki, &[]).await;
    let (error, closed, _) = peer.read_until_closed(Duration::from_millis(100)).await;
    assert!(!closed, "the deployment closed the stream");
    assert_eq!(error, None);
    // The first half of a stanza, then the socket closes.
    let stanza = peer.encode(&into_server_content(message("cut-off".to_owned(), 256)));
    peer.write(&stanza[..stanza.len() / 2])
        .await
        .expect("write part of a stanza");
    drop(peer);
    assert_eq!(
        endings(&sink).await,
        vec![(StreamDirection::Inbound, HOME_A.to_owned(), StreamEnd::Lost)]
    );
    assert!(eventually(WAIT, || b.handle.streams().is_empty()).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_peers_connection_timeout_is_closed() {
    let pki = Pki::new();
    let (b, sink, mut peer) = inbound(&pki, &[]).await;
    // The peer ends an idle stream as RFC 6120 §4.9.3.4 describes.
    let error = peer.stream_error(StreamErrorCondition::ConnectionTimeout);
    peer.write(&error).await.expect("write the stream error");
    let (_, closed, _) = peer.read_until_closed(WAIT).await;
    assert!(closed, "the deployment did not close the stream");
    drop(peer);
    assert_eq!(
        endings(&sink).await,
        vec![(
            StreamDirection::Inbound,
            HOME_A.to_owned(),
            StreamEnd::Closed
        )]
    );
    assert!(eventually(WAIT, || b.handle.streams().is_empty()).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn another_stream_error_from_the_peer_is_lost() {
    let pki = Pki::new();
    let (b, sink, mut peer) = inbound(&pki, &[]).await;
    let error = peer.stream_error(StreamErrorCondition::InternalServerError);
    peer.write(&error).await.expect("write the stream error");
    let (_, closed, _) = peer.read_until_closed(WAIT).await;
    assert!(closed, "the deployment did not close the stream");
    drop(peer);
    assert_eq!(
        endings(&sink).await,
        vec![(StreamDirection::Inbound, HOME_A.to_owned(), StreamEnd::Lost)]
    );
    assert!(eventually(WAIT, || b.handle.streams().is_empty()).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_peers_closing_tag_is_closed() {
    let pki = Pki::new();
    let (b, sink, mut peer) = inbound(&pki, &[]).await;
    peer.write(&closing_tag())
        .await
        .expect("write the closing stream tag");
    let (error, closed, _) = peer.read_until_closed(WAIT).await;
    assert!(closed, "the deployment did not close the stream");
    assert_eq!(error, None);
    drop(peer);
    assert_eq!(
        endings(&sink).await,
        vec![(
            StreamDirection::Inbound,
            HOME_A.to_owned(),
            StreamEnd::Closed
        )]
    );
    assert!(eventually(WAIT, || b.handle.streams().is_empty()).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_element_left_unfinished_past_its_deadline_is_lost() {
    let pki = Pki::new();
    let incomplete = format!("incomplete_element_timeout_ms = {SHORT_DEADLINE_MS}");
    let (b, sink, mut peer) = inbound(&pki, &[incomplete.as_str()]).await;
    // The first half of a stanza, then nothing: far less than the input
    // burst, so no read waits for the input rate.
    let stanza = peer.encode(&into_server_content(message("unfinished".to_owned(), 256)));
    peer.write(&stanza[..stanza.len() / 2])
        .await
        .expect("write part of a stanza");
    let (error, closed, _) = peer.read_until_closed(WAIT).await;
    assert!(closed, "the deployment did not close the stream");
    assert_eq!(error, Some(StreamErrorCondition::ConnectionTimeout));
    assert_eq!(
        endings(&sink).await,
        vec![(StreamDirection::Inbound, HOME_A.to_owned(), StreamEnd::Lost)]
    );
    assert!(eventually(WAIT, || b.handle.streams().is_empty()).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_stream_with_an_element_unfinished_is_lost() {
    let pki = Pki::new();
    // The idle timeout comes well before the incomplete-element deadline
    // (five seconds by default).
    let idle = format!("idle_timeout_ms = {SHORT_IDLE_MS}");
    let (b, sink, mut peer) = inbound(&pki, &[idle.as_str()]).await;
    let stanza = peer.encode(&into_server_content(message("idle".to_owned(), 256)));
    peer.write(&stanza[..stanza.len() / 2])
        .await
        .expect("write part of a stanza");
    let (error, closed, _) = peer.read_until_closed(WAIT).await;
    assert!(closed, "the deployment did not close the idle stream");
    assert_eq!(error, Some(StreamErrorCondition::ConnectionTimeout));
    assert_eq!(
        endings(&sink).await,
        vec![(StreamDirection::Inbound, HOME_A.to_owned(), StreamEnd::Lost)]
    );
    assert!(eventually(WAIT, || b.handle.streams().is_empty()).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_maximum_age_with_an_element_unfinished_is_lost() {
    let pki = Pki::new();
    // The maximum age comes well before the incomplete-element deadline
    // (five seconds by default) and the idle timeout.
    let max_age = format!("max_age_ms = {SHORT_IDLE_MS}");
    let (b, sink, mut peer) = inbound(&pki, &[max_age.as_str()]).await;
    let stanza = peer.encode(&into_server_content(message("aged".to_owned(), 256)));
    peer.write(&stanza[..stanza.len() / 2])
        .await
        .expect("write part of a stanza");
    let (error, closed, _) = peer.read_until_closed(WAIT).await;
    assert!(closed, "the deployment did not close the stream");
    assert_eq!(error, Some(StreamErrorCondition::Reset));
    assert_eq!(
        endings(&sink).await,
        vec![(StreamDirection::Inbound, HOME_A.to_owned(), StreamEnd::Lost)]
    );
    assert!(eventually(WAIT, || b.handle.streams().is_empty()).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_maximum_age_passing_during_a_wait_for_the_sink_is_lost() {
    let pki = Pki::new();
    let max_age = format!("max_age_ms = {SHORT_IDLE_MS}");
    let sink = Arc::new(HoldingSink::new());
    let (b, mut peer) = inbound_to(&pki, &[max_age.as_str()], sink.clone()).await;
    let held = peer.encode(&into_server_content(message(HELD.to_owned(), 64)));
    peer.write(&held).await.expect("write the held stanza");
    assert!(
        eventually(WAIT, || sink.offered() == [HELD]).await,
        "the held stanza was not offered"
    );
    // Written while the deployment reads nothing: it waits in the
    // transport.
    let behind = peer.encode(&into_server_content(message("behind".to_owned(), 64)));
    peer.write(&behind)
        .await
        .expect("write a stanza behind the held one");
    tokio::time::sleep(Duration::from_millis(SHORT_IDLE_MS * 2)).await;
    sink.release();
    let (error, closed, _) = peer.read_until_closed(WAIT).await;
    assert!(closed, "the deployment did not close the stream");
    assert_eq!(error, Some(StreamErrorCondition::Reset));
    // The stanza behind the held one was never read, so the stream lost it.
    assert_eq!(sink.offered(), [HELD]);
    assert_eq!(
        endings(&sink.endings).await,
        vec![(StreamDirection::Inbound, HOME_A.to_owned(), StreamEnd::Lost)]
    );
    assert!(eventually(WAIT, || b.handle.streams().is_empty()).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_wait_for_the_sink_longer_than_the_idle_timeout_keeps_the_stream() {
    let pki = Pki::new();
    let idle = format!("idle_timeout_ms = {SHORT_IDLE_MS}");
    let sink = Arc::new(HoldingSink::new());
    let (b, mut peer) = inbound_to(&pki, &[idle.as_str()], sink.clone()).await;
    let held = peer.encode(&into_server_content(message(HELD.to_owned(), 64)));
    peer.write(&held).await.expect("write the held stanza");
    assert!(
        eventually(WAIT, || sink.offered() == [HELD]).await,
        "the held stanza was not offered"
    );
    let behind = peer.encode(&into_server_content(message("behind".to_owned(), 64)));
    peer.write(&behind)
        .await
        .expect("write a stanza behind the held one");
    // Well past the idle timeout, counted from the held stanza.
    tokio::time::sleep(Duration::from_millis(SHORT_IDLE_MS * 2)).await;
    assert!(
        sink.endings.endings().is_empty(),
        "the stream ended during the wait"
    );
    sink.release();
    // The stanza the sink took counts as traffic, so the stream reads on
    // instead of ending idle at once.
    assert!(
        eventually(WAIT, || sink.offered() == [HELD, "behind"]).await,
        "the stanza behind the held one was not read: {:?}",
        sink.offered()
    );
    // Once it has read again and then idles, the stream ends in an
    // orderly way.
    let (error, closed, _) = peer.read_until_closed(WAIT).await;
    assert!(closed, "the deployment did not close the idle stream");
    assert_eq!(error, Some(StreamErrorCondition::ConnectionTimeout));
    assert_eq!(
        endings(&sink.endings).await,
        vec![(
            StreamDirection::Inbound,
            HOME_A.to_owned(),
            StreamEnd::Closed
        )]
    );
    assert!(eventually(WAIT, || b.handle.streams().is_empty()).await);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_idle_stream_with_nothing_unfinished_is_closed() {
    let pki = Pki::new();
    let idle = format!("idle_timeout_ms = {SHORT_IDLE_MS}");
    let (b, sink, mut peer) = inbound(&pki, &[idle.as_str()]).await;
    let (error, closed, _) = peer.read_until_closed(WAIT).await;
    assert!(closed, "the deployment did not close the idle stream");
    assert_eq!(error, Some(StreamErrorCondition::ConnectionTimeout));
    assert_eq!(
        endings(&sink).await,
        vec![(
            StreamDirection::Inbound,
            HOME_A.to_owned(),
            StreamEnd::Closed
        )]
    );
    assert!(eventually(WAIT, || b.handle.streams().is_empty()).await);
}

/// A TCP relay for one connection to `target`: what `target` sends is
/// copied back throughout, and what the dialler sends is copied to
/// `target` until `stall` is cancelled. From then on the relay reads
/// nothing more from the dialler and keeps both connections open, so the
/// dialler's writes back up once the socket buffers are full.
async fn relay(target: SocketAddr, stall: CancellationToken) -> SocketAddr {
    let socket = TcpSocket::new_v4().expect("a socket");
    // Accepted sockets inherit the listening socket's receive buffer.
    socket
        .set_recv_buffer_size(RELAY_RECEIVE_BUFFER)
        .expect("a receive buffer size");
    socket
        .bind(SocketAddr::from(([127, 0, 0, 1], 0)))
        .expect("bind a loopback port");
    let listener = socket.listen(4).expect("listen");
    let address = listener.local_addr().expect("a bound address");
    tokio::spawn(async move {
        let Ok((dialler, _)) = listener.accept().await else {
            return;
        };
        let Ok(target) = TcpStream::connect(target).await else {
            return;
        };
        let (from_dialler, to_dialler) = dialler.into_split();
        let (from_target, to_target) = target.into_split();
        tokio::spawn(copy_back(from_target, to_dialler));
        forward_until_stalled(from_dialler, to_target, stall).await;
    });
    address
}

async fn copy_back(mut from: OwnedReadHalf, mut to: OwnedWriteHalf) {
    let _ = tokio::io::copy(&mut from, &mut to).await;
}

async fn forward_until_stalled(
    mut from: OwnedReadHalf,
    mut to: OwnedWriteHalf,
    stall: CancellationToken,
) {
    let mut chunk = vec![0u8; 16 * 1024];
    loop {
        let read = tokio::select! {
            biased;
            () = stall.cancelled() => break,
            read = from.read(&mut chunk) => read,
        };
        match read {
            Ok(0) | Err(_) => return,
            Ok(read) => {
                if to.write_all(&chunk[..read]).await.is_err() {
                    return;
                }
            }
        }
    }
    // Stalled: hold both halves open and read nothing more.
    let _held = (from, to);
    std::future::pending::<()>().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_write_past_the_write_deadline_is_lost() {
    let pki = Pki::new();
    let (listener_b, address_b) = listener().await;
    let b = start(&pki, &Spec::new(HOME_B, HOME_A), listener_b).await;
    let stall = CancellationToken::new();
    let relay_address = relay(address_b, stall.clone()).await;

    let sink = Arc::new(EndingSink::default());
    let (listener_a, _) = listener().await;
    let mut spec_a = Spec::new(HOME_A, HOME_B);
    spec_a.endpoints = vec![(HOME_B, relay_address)];
    spec_a.limits = vec![format!("write_timeout_ms = {SHORT_DEADLINE_MS}")];
    let options = ServiceOptions {
        sink: sink.clone(),
        ..ServiceOptions::default()
    };
    let a = start_with(&pki, &spec_a, listener_a, options).await;
    let stream = a
        .handle
        .connect(&domain(HOME_A), &domain(HOME_B))
        .await
        .expect("an outbound stream through the relay");
    holds_streams(&b, 1).await;
    stall.cancel();

    // Each write either completes into the socket buffers or, once they
    // are full, runs into the write deadline and ends the stream.
    let failed = tokio::time::timeout(Duration::from_secs(60), async {
        for index in 0..MAX_BULK_STANZAS {
            let stanza = message(format!("bulk-{index}"), BULK_BODY_BYTES);
            if let Err(error) = a.handle.transmit(&stream, stanza).await {
                return Some(error);
            }
        }
        None
    })
    .await
    .expect("every write ends within its deadline");
    let error = failed.expect("a relay that stopped reading ends the stream");
    assert!(matches!(error, TransmitError::Unknown), "{error:?}");
    assert_eq!(
        endings(&sink).await,
        vec![(
            StreamDirection::Outbound,
            HOME_B.to_owned(),
            StreamEnd::Lost
        )]
    );
    assert!(eventually(WAIT, || a.handle.streams().is_empty()).await);
}
