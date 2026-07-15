use futures::future::BoxFuture;
use futures::stream::{SplitSink, SplitStream};
use futures::{SinkExt, StreamExt};
use std::collections::VecDeque;
use std::sync::{Arc, OnceLock};
use tokio::net::TcpStream;
use tokio::time::timeout;
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
use tokio_tungstenite::tungstenite::http::Request;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::{
    connect_async_tls_with_config, Connector, MaybeTlsStream, WebSocketStream,
};

use crate::config::ClientConfig;
use crate::error::{ClientError, ClientResult};

use super::{
    decode_message, encode_message, StreamClose, TransportCapabilities, TransportEvent,
    TransportKind, TransportMessage, TransportState,
};

/// Whether a failed native transport write can still have transferred the
/// typed XMPP frame to the peer.
///
/// Once a WebSocket sink starts a write, an error cannot prove that zero bytes
/// reached the network. XEP-0198 therefore has to retain responsibility for a
/// countable stanza after [`Self::PossiblyWritten`]. Encoding and preflight
/// failures happen before the sink is touched and are
/// [`Self::DefinitelyNotWritten`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportWriteResponsibility {
    DefinitelyNotWritten,
    PossiblyWritten,
}

/// Typed failure returned by the native write boundary.
#[derive(Debug)]
pub struct TransportWriteFailure {
    responsibility: TransportWriteResponsibility,
    source: ClientError,
}

impl TransportWriteFailure {
    pub fn definitely_not_written(source: ClientError) -> Self {
        Self {
            responsibility: TransportWriteResponsibility::DefinitelyNotWritten,
            source,
        }
    }

    pub fn possibly_written(source: ClientError) -> Self {
        Self {
            responsibility: TransportWriteResponsibility::PossiblyWritten,
            source,
        }
    }

    pub fn responsibility(&self) -> TransportWriteResponsibility {
        self.responsibility
    }

    pub fn source(&self) -> &ClientError {
        &self.source
    }
}

pub type TransportWriteResult<T> = Result<T, TransportWriteFailure>;

/// Runtime-owned factory for a WebSocket transport implementation.
pub trait WebSocketTransportFactory: Send + Sync {
    fn connect<'a>(
        &'a self,
        config: &'a ClientConfig,
    ) -> BoxFuture<'a, ClientResult<Box<dyn WebSocketTransport>>>;
}

/// Minimal async boundary for the WebSocket transport layer.
pub trait WebSocketTransport: Send + Sync {
    fn kind(&self) -> TransportKind {
        TransportKind::WebSocket
    }

    fn capabilities(&self) -> TransportCapabilities {
        TransportCapabilities::default()
    }

    fn drain_events(&mut self) -> Vec<TransportEvent>;

    /// Write one typed transport message.
    ///
    /// Implementations must not enqueue `TransportEvent::MessageSent`; the
    /// owning driver applies that event exactly once after this future confirms
    /// success. This keeps transport responsibility and XEP-0198 queue state in
    /// one ordered pump.
    fn send<'a>(&'a mut self, message: TransportMessage)
        -> BoxFuture<'a, TransportWriteResult<()>>;

    fn next_event<'a>(&'a mut self) -> BoxFuture<'a, ClientResult<Option<TransportEvent>>>;

    fn close<'a>(&'a mut self) -> BoxFuture<'a, TransportWriteResult<()>>;

    /// Terminate the WebSocket without sending RFC 7395 `<close/>`.
    /// XEP-0198 treats this as an unclean XML-stream loss, so the runtime's
    /// resume snapshot remains eligible for the reconnecting owner.
    fn abort<'a>(&'a mut self) -> BoxFuture<'a, ClientResult<()>>;
}

/// Default runtime factory for the concrete WebSocket transport.
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultTransportFactory;

impl WebSocketTransportFactory for DefaultTransportFactory {
    fn connect<'a>(
        &'a self,
        config: &'a ClientConfig,
    ) -> BoxFuture<'a, ClientResult<Box<dyn WebSocketTransport>>> {
        Box::pin(async move {
            let request = websocket_request(config)?;
            let connector = rustls_connector()?;
            let connect_timeout = config.transport.connect_timeout;
            let (socket, _) = timeout(
                connect_timeout,
                connect_async_tls_with_config(request, None, false, Some(connector)),
            )
            .await
            .map_err(|_| ClientError::WebSocketConnectTimeout {
                timeout: connect_timeout,
            })??;

            Ok(Box::new(ConnectedWebSocketTransport::new(socket)) as Box<dyn WebSocketTransport>)
        })
    }
}

type ClientWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
type ClientWebSocketSink = SplitSink<ClientWebSocket, Message>;
type ClientWebSocketStream = SplitStream<ClientWebSocket>;

#[derive(Debug)]
struct ConnectedWebSocketTransport {
    sink: ClientWebSocketSink,
    stream: ClientWebSocketStream,
    state: TransportState,
    pending_events: VecDeque<TransportEvent>,
}

impl ConnectedWebSocketTransport {
    fn new(socket: ClientWebSocket) -> Self {
        let mut pending_events = VecDeque::with_capacity(2);
        pending_events.push_back(TransportEvent::StateChanged(TransportState::Connecting));
        pending_events.push_back(TransportEvent::StateChanged(TransportState::Open));

        let (sink, stream) = socket.split();

        Self {
            sink,
            stream,
            state: TransportState::Open,
            pending_events,
        }
    }

    fn queue_state_change(&mut self, state: TransportState) {
        if self.state != state {
            self.state = state;
            self.pending_events
                .push_back(TransportEvent::StateChanged(state));
        }
    }

    fn queue_closed(&mut self) {
        if self.state != TransportState::Closed {
            self.state = TransportState::Closed;
            self.pending_events
                .push_back(TransportEvent::StateChanged(TransportState::Closed));
            self.pending_events.push_back(TransportEvent::Closed);
        }
    }
}

impl WebSocketTransport for ConnectedWebSocketTransport {
    fn drain_events(&mut self) -> Vec<TransportEvent> {
        self.pending_events.drain(..).collect()
    }

    fn send<'a>(
        &'a mut self,
        message: TransportMessage,
    ) -> BoxFuture<'a, TransportWriteResult<()>> {
        Box::pin(async move {
            if self.state == TransportState::Closed {
                return Err(TransportWriteFailure::definitely_not_written(
                    ClientError::TransportClosed,
                ));
            }

            let frame =
                encode_message(&message).map_err(TransportWriteFailure::definitely_not_written)?;
            self.sink
                .send(Message::Text(frame.into()))
                .await
                .map_err(|error| TransportWriteFailure::possibly_written(error.into()))?;

            if matches!(message, TransportMessage::Close(_)) {
                self.queue_state_change(TransportState::Closing);
            }
            Ok(())
        })
    }

    fn next_event<'a>(&'a mut self) -> BoxFuture<'a, ClientResult<Option<TransportEvent>>> {
        Box::pin(async move {
            if let Some(event) = self.pending_events.pop_front() {
                return Ok(Some(event));
            }

            loop {
                let inbound = self.stream.next().await;

                match inbound {
                    Some(Ok(Message::Text(text))) => {
                        let message = decode_message(text.as_ref())?;
                        if matches!(message, TransportMessage::Close(_)) {
                            self.queue_state_change(TransportState::Closing);
                        }
                        self.pending_events
                            .push_back(TransportEvent::MessageReceived(message));
                        return Ok(self.pending_events.pop_front());
                    }
                    Some(Ok(Message::Ping(payload))) => {
                        self.sink.send(Message::Pong(payload)).await?;
                    }
                    Some(Ok(Message::Pong(_))) => {}
                    Some(Ok(Message::Close(_))) => {
                        self.queue_closed();
                        return Ok(self.pending_events.pop_front());
                    }
                    Some(Ok(Message::Binary(_))) => {
                        self.queue_state_change(TransportState::Failed);
                        return Err(ClientError::UnsupportedWebSocketMessage);
                    }
                    Some(Ok(_)) => {}
                    Some(Err(err)) => {
                        self.queue_state_change(TransportState::Failed);
                        return Err(err.into());
                    }
                    None => {
                        self.queue_closed();
                        return Ok(self.pending_events.pop_front());
                    }
                }
            }
        })
    }

    fn close<'a>(&'a mut self) -> BoxFuture<'a, TransportWriteResult<()>> {
        Box::pin(async move {
            if self.state == TransportState::Closed {
                return Ok(());
            }

            if self.state != TransportState::Closing {
                self.queue_state_change(TransportState::Closing);
                let frame = encode_message(&TransportMessage::Close(StreamClose))
                    .map_err(TransportWriteFailure::definitely_not_written)?;
                self.sink
                    .send(Message::Text(frame.into()))
                    .await
                    .map_err(|error| TransportWriteFailure::possibly_written(error.into()))?;
            }

            self.sink
                .close()
                .await
                .map_err(|error| TransportWriteFailure::possibly_written(error.into()))?;
            self.queue_closed();
            Ok(())
        })
    }

    fn abort<'a>(&'a mut self) -> BoxFuture<'a, ClientResult<()>> {
        Box::pin(async move {
            if self.state == TransportState::Closed {
                return Ok(());
            }
            self.queue_state_change(TransportState::Closing);
            let result: ClientResult<()> = self.sink.close().await.map_err(Into::into);
            if result.is_err() {
                self.queue_state_change(TransportState::Failed);
            }
            self.queue_closed();
            result
        })
    }
}

/// Builds the TLS connector for `wss://` endpoints (ignored for `ws://`).
///
/// The connector is always rustls with the bundled webpki (Mozilla) roots and
/// the `ring` crypto provider. Passing it explicitly — instead of relying on
/// tokio-tungstenite's auto-connector — is load-bearing: in whole-workspace
/// builds cargo unions our rustls features with the `native-tls` feature other
/// workspace members enable, and the auto-connector would then prefer
/// native-tls, silently diverging from the Android/iOS builds.
///
/// The config is process-cached: parsing the ~150 webpki roots on every
/// reconnect is wasted work on mobile, where reconnects are frequent. Two
/// racing first callers may build it twice; `get_or_init` keeps one.
pub(super) fn rustls_connector() -> ClientResult<Connector> {
    static TLS_CONFIG: OnceLock<Arc<rustls::ClientConfig>> = OnceLock::new();

    if let Some(config) = TLS_CONFIG.get() {
        return Ok(Connector::Rustls(Arc::clone(config)));
    }

    let mut roots = rustls::RootCertStore::empty();
    roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());

    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let config = Arc::new(
        rustls::ClientConfig::builder_with_provider(provider)
            .with_safe_default_protocol_versions()
            .map_err(ClientError::WebSocketTlsConfig)?
            .with_root_certificates(roots)
            .with_no_client_auth(),
    );

    Ok(Connector::Rustls(Arc::clone(
        TLS_CONFIG.get_or_init(|| config),
    )))
}

fn websocket_request(config: &ClientConfig) -> ClientResult<Request<()>> {
    let host = websocket_host_header(&config.transport.endpoint)?;
    let mut builder = Request::builder()
        .uri(config.transport.endpoint.as_str())
        .header("Host", host)
        .header("Connection", "Upgrade")
        .header("Upgrade", "websocket")
        .header("Sec-WebSocket-Version", "13")
        .header("Sec-WebSocket-Key", generate_key())
        .header("Sec-WebSocket-Protocol", "xmpp");

    if let Some(origin) = &config.transport.origin {
        builder = builder.header("Origin", origin.as_str());
    }

    builder
        .body(())
        .map_err(ClientError::InvalidWebSocketRequest)
}

fn websocket_host_header(endpoint: &url::Url) -> ClientResult<String> {
    let host = endpoint
        .host_str()
        .ok_or(ClientError::MissingWebSocketHost)?;

    Ok(match endpoint.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}
