//! Loopback WebSocket client harness shared by the SASL2 and Bind 2 suites:
//! one in-memory connection to the production router, framing, and the
//! OAUTHBEARER initial response.
use axum::serve::Listener;
use futures::{SinkExt, StreamExt};
use minidom::Element;
use std::future;
use std::io;
use std::sync::Arc;
use std::time::Duration;
use tokio::io::DuplexStream;
use tokio::task::JoinHandle;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;
use waddle_server::test_support::{router, WebSocketState};

pub const TIMEOUT: Duration = Duration::from_secs(5);
pub const NS_FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
pub const NS_STREAM: &str = "http://etherx.jabber.org/streams";

pub type Peer = WebSocketStream<DuplexStream>;

struct OneConnection(Option<DuplexStream>);

impl Listener for OneConnection {
    type Io = DuplexStream;
    type Addr = ();

    async fn accept(&mut self) -> (Self::Io, Self::Addr) {
        match self.0.take() {
            Some(io) => (io, ()),
            None => future::pending().await,
        }
    }

    fn local_addr(&self) -> io::Result<Self::Addr> {
        Ok(())
    }
}

pub async fn connect(state: Arc<WebSocketState>) -> (Peer, JoinHandle<()>) {
    let (server_io, peer_io) = tokio::io::duplex(8192);
    let server = tokio::spawn(async move {
        axum::serve(OneConnection(Some(server_io)), router(state))
            .await
            .expect("serve");
    });
    let (peer, response) = tokio_tungstenite::client_async("ws://localhost/ws", peer_io)
        .await
        .expect("websocket upgrade");
    assert_eq!(response.status(), 101);
    (peer, server)
}

pub async fn send(peer: &mut Peer, element: Element) {
    peer.send(Message::Text(String::from(&element).into()))
        .await
        .expect("send frame");
}

pub async fn receive(peer: &mut Peer) -> Element {
    match timeout(TIMEOUT, peer.next()).await.expect("frame timeout") {
        Some(Ok(Message::Text(text))) => text.parse().expect("XML frame"),
        other => panic!("expected text frame, got {other:?}"),
    }
}

/// Open the stream and return its pre-authentication features.
pub async fn start_stream(peer: &mut Peer) -> Element {
    send(
        peer,
        Element::builder("open", NS_FRAMING)
            .attr(minidom::rxml::xml_ncname!("to").to_owned(), "example.com")
            .attr(minidom::rxml::xml_ncname!("version").to_owned(), "1.0")
            .build(),
    )
    .await;
    assert_eq!(receive(peer).await.name(), "open");
    let features = receive(peer).await;
    assert_eq!(
        (features.name(), features.ns()),
        ("features", NS_STREAM.to_owned())
    );
    features
}

pub fn bearer(token: &str) -> Vec<u8> {
    let mut payload = b"n,,".to_vec();
    payload.push(1);
    payload.extend_from_slice(b"auth=Bearer ");
    payload.extend_from_slice(token.as_bytes());
    payload.extend_from_slice(b"\x01\x01");
    payload
}

pub async fn expect_post_auth_features(peer: &mut Peer) {
    let features = receive(peer).await;
    assert_eq!(
        (features.name(), features.ns()),
        ("features", NS_STREAM.to_owned())
    );
}

/// Connect and open the stream.
pub async fn connect_started(state: Arc<WebSocketState>) -> (Peer, JoinHandle<()>) {
    let (mut peer, server) = connect(state).await;
    start_stream(&mut peer).await;
    (peer, server)
}
