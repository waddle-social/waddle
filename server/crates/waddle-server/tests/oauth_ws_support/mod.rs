//! In-process WebSocket harness for the OAUTHBEARER suites: one duplex
//! connection to the production router, and the frames of an RFC 7395
//! stream with SASL OAUTHBEARER.

use axum::serve::Listener;
use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
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

const TIMEOUT: Duration = Duration::from_secs(5);
const NS_FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
const NS_SASL: &str = "urn:ietf:params:xml:ns:xmpp-sasl";
const NS_STREAM: &str = "http://etherx.jabber.org/streams";

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

pub fn open(from: Option<&str>) -> Element {
    let mut builder = Element::builder("open", NS_FRAMING)
        .attr(minidom::rxml::xml_ncname!("to").to_owned(), "example.com")
        .attr(minidom::rxml::xml_ncname!("version").to_owned(), "1.0");
    if let Some(from) = from {
        builder = builder.attr(minidom::rxml::xml_ncname!("from").to_owned(), from);
    }
    builder.build()
}

pub fn oauth(data: &[u8]) -> Element {
    Element::builder("auth", NS_SASL)
        .attr(
            minidom::rxml::xml_ncname!("mechanism").to_owned(),
            "OAUTHBEARER",
        )
        .append(BASE64_STANDARD.encode(data))
        .build()
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

pub async fn start_stream(peer: &mut Peer, from: Option<&str>) -> Element {
    send(peer, open(from)).await;
    let response = receive(peer).await;
    assert_eq!(
        (response.name(), response.ns()),
        ("open", NS_FRAMING.to_owned())
    );
    let features = receive(peer).await;
    assert_eq!(
        (features.name(), features.ns()),
        ("features", NS_STREAM.to_owned())
    );
    response
}

pub fn bearer(token: &str, authzid: Option<&str>) -> Vec<u8> {
    let mut payload = match authzid {
        Some(value) => format!("n,a={value},").into_bytes(),
        None => b"n,,".to_vec(),
    };
    payload.push(1);
    payload.extend_from_slice(b"auth=Bearer ");
    payload.extend_from_slice(token.as_bytes());
    payload.extend_from_slice(b"\x01\x01");
    payload
}
