use jid::BareJid;
use minidom::Element;
use std::str::FromStr;

use crate::bootstrap::NS_CLIENT;
use crate::config::ClientConfig;
use crate::error::{ClientError, ClientResult};
use crate::state::StreamId;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use futures::future::BoxFuture;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use futures::stream::{SplitSink, SplitStream};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use futures::{SinkExt, StreamExt};
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use std::collections::VecDeque;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use tokio::net::TcpStream;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use tokio::time::timeout;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use tokio_tungstenite::tungstenite::handshake::client::generate_key;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use tokio_tungstenite::tungstenite::http::Request;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use tokio_tungstenite::tungstenite::Message;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
use tokio_tungstenite::{connect_async, MaybeTlsStream, WebSocketStream};

const NS_XMPP_FRAMING: &str = "urn:ietf:params:xml:ns:xmpp-framing";
const MAX_FRAME_SIZE: usize = 1024 * 1024;

/// Transport kind supported by the native client runtime.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportKind {
    WebSocket,
}

/// Connection state surfaced by the transport adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransportState {
    Idle,
    Connecting,
    Open,
    Closing,
    Closed,
    Failed,
}

/// Static feature flags reported by a connected transport.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TransportCapabilities {
    pub stream_management: bool,
    pub resumable: bool,
}

impl Default for TransportCapabilities {
    fn default() -> Self {
        Self {
            stream_management: true,
            resumable: true,
        }
    }
}

/// RFC 7395 `<open/>` frame.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamOpen {
    pub to: Option<BareJid>,
    pub from: Option<BareJid>,
    pub id: Option<StreamId>,
    pub language: Option<String>,
    pub version: &'static str,
}

impl StreamOpen {
    pub const VERSION: &'static str = "1.0";

    pub fn from_config(config: &ClientConfig) -> Self {
        Self {
            to: Some(config.connection.server.clone()),
            from: None,
            id: None,
            language: config.session.language.clone(),
            version: Self::VERSION,
        }
    }

    pub fn from_server(from: BareJid) -> Self {
        Self {
            to: None,
            from: Some(from),
            id: None,
            language: None,
            version: Self::VERSION,
        }
    }
}

/// RFC 7395 `<close/>` frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StreamClose;

/// Typed transport payloads exchanged across the runtime boundary.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportMessage {
    Open(StreamOpen),
    Element(Element),
    Close(StreamClose),
}

/// Transport-level events surfaced back into the session runtime.
#[derive(Debug, Clone, PartialEq)]
pub enum TransportEvent {
    StateChanged(TransportState),
    MessageReceived(TransportMessage),
    MessageSent(TransportMessage),
    Closed,
}

impl TransportEvent {
    pub fn transport_state(&self) -> TransportState {
        match self {
            Self::StateChanged(state) => *state,
            Self::MessageReceived(TransportMessage::Close(_))
            | Self::MessageSent(TransportMessage::Close(_)) => TransportState::Closing,
            Self::MessageReceived(_) | Self::MessageSent(_) => TransportState::Open,
            Self::Closed => TransportState::Closed,
        }
    }
}

/// Runtime-owned factory for a WebSocket transport implementation.
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub trait WebSocketTransportFactory: Send + Sync {
    fn connect<'a>(
        &'a self,
        config: &'a ClientConfig,
    ) -> BoxFuture<'a, ClientResult<Box<dyn WebSocketTransport>>>;
}

/// Minimal async boundary for the WebSocket transport layer.
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
pub trait WebSocketTransport: Send + Sync {
    fn kind(&self) -> TransportKind {
        TransportKind::WebSocket
    }

    fn capabilities(&self) -> TransportCapabilities {
        TransportCapabilities::default()
    }

    fn drain_events(&mut self) -> Vec<TransportEvent>;

    fn send<'a>(&'a mut self, message: TransportMessage) -> BoxFuture<'a, ClientResult<()>>;

    fn next_event<'a>(&'a mut self) -> BoxFuture<'a, ClientResult<Option<TransportEvent>>>;

    fn close<'a>(&'a mut self) -> BoxFuture<'a, ClientResult<()>>;
}

/// Default runtime factory for the concrete WebSocket transport.
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
#[derive(Debug, Default, Clone, Copy)]
pub struct DefaultTransportFactory;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl WebSocketTransportFactory for DefaultTransportFactory {
    fn connect<'a>(
        &'a self,
        config: &'a ClientConfig,
    ) -> BoxFuture<'a, ClientResult<Box<dyn WebSocketTransport>>> {
        Box::pin(async move {
            let request = websocket_request(config)?;
            let connect_timeout = config.transport.connect_timeout;
            let (socket, _) = timeout(connect_timeout, connect_async(request))
                .await
                .map_err(|_| ClientError::WebSocketConnectTimeout {
                    timeout: connect_timeout,
                })??;

            Ok(Box::new(ConnectedWebSocketTransport::new(socket)) as Box<dyn WebSocketTransport>)
        })
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
type ClientWebSocket = WebSocketStream<MaybeTlsStream<TcpStream>>;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
type ClientWebSocketSink = SplitSink<ClientWebSocket, Message>;
#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
type ClientWebSocketStream = SplitStream<ClientWebSocket>;

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
#[derive(Debug)]
struct ConnectedWebSocketTransport {
    sink: ClientWebSocketSink,
    stream: ClientWebSocketStream,
    state: TransportState,
    pending_events: VecDeque<TransportEvent>,
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
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

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
impl WebSocketTransport for ConnectedWebSocketTransport {
    fn drain_events(&mut self) -> Vec<TransportEvent> {
        self.pending_events.drain(..).collect()
    }

    fn send<'a>(&'a mut self, message: TransportMessage) -> BoxFuture<'a, ClientResult<()>> {
        Box::pin(async move {
            if self.state == TransportState::Closed {
                return Err(ClientError::TransportClosed);
            }

            let frame = encode_message(&message)?;
            self.sink.send(Message::Text(frame)).await?;

            if matches!(message, TransportMessage::Close(_)) {
                self.queue_state_change(TransportState::Closing);
            }
            self.pending_events
                .push_back(TransportEvent::MessageSent(message));
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

    fn close<'a>(&'a mut self) -> BoxFuture<'a, ClientResult<()>> {
        Box::pin(async move {
            if self.state == TransportState::Closed {
                return Ok(());
            }

            if self.state != TransportState::Closing {
                self.queue_state_change(TransportState::Closing);
                let frame = encode_message(&TransportMessage::Close(StreamClose))?;
                self.sink.send(Message::Text(frame)).await?;
                self.pending_events.push_back(TransportEvent::MessageSent(
                    TransportMessage::Close(StreamClose),
                ));
            }

            self.sink.close().await?;
            self.queue_closed();
            Ok(())
        })
    }
}

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
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

#[cfg(all(feature = "native", not(target_arch = "wasm32")))]
fn websocket_host_header(endpoint: &url::Url) -> ClientResult<String> {
    let host = endpoint
        .host_str()
        .ok_or(ClientError::MissingWebSocketHost)?;

    Ok(match endpoint.port() {
        Some(port) => format!("{host}:{port}"),
        None => host.to_string(),
    })
}

/// Encode a typed transport message as one RFC 7395 WebSocket text frame.
///
/// Browser integrations can use this with a JavaScript-owned `WebSocket`
/// while keeping XMPP stream state in [`crate::runtime::XmppRuntime`].
pub fn encode_message(message: &TransportMessage) -> ClientResult<String> {
    let encoded = match message {
        TransportMessage::Open(open) => serialize_element(&stream_open_element(open))?,
        TransportMessage::Element(element) => serialize_element(element)?,
        TransportMessage::Close(_) => {
            serialize_element(&Element::builder("close", NS_XMPP_FRAMING).build())?
        }
    };

    if encoded.len() > MAX_FRAME_SIZE {
        return Err(ClientError::TransportFrameTooLarge {
            max: MAX_FRAME_SIZE,
        });
    }

    Ok(encoded)
}

fn serialize_element(element: &Element) -> ClientResult<String> {
    let mut buffer = Vec::new();
    element
        .write_to(&mut buffer)
        .map_err(|_| ClientError::InvalidTransportFrame)?;
    String::from_utf8(buffer).map_err(|_| ClientError::InvalidTransportFrame)
}

fn stream_open_element(open: &StreamOpen) -> Element {
    let mut builder = Element::builder("open", NS_XMPP_FRAMING).attr("version", open.version);
    if let Some(to) = &open.to {
        builder = builder.attr("to", to.to_string());
    }
    if let Some(from) = &open.from {
        builder = builder.attr("from", from.to_string());
    }
    if let Some(id) = &open.id {
        builder = builder.attr("id", id.as_str());
    }
    if let Some(language) = &open.language {
        builder = builder.attr("xml:lang", language);
    }
    builder.build()
}

/// Decode one RFC 7395 WebSocket text frame into a typed transport message.
///
/// Incoming bare client stanzas without an explicit default namespace are
/// normalised to `jabber:client`, matching the native transport.
pub fn decode_message(frame: &str) -> ClientResult<TransportMessage> {
    let trimmed = frame.trim();
    if trimmed.is_empty() {
        return Err(ClientError::EmptyTransportFrame);
    }
    if trimmed.len() > MAX_FRAME_SIZE {
        return Err(ClientError::TransportFrameTooLarge {
            max: MAX_FRAME_SIZE,
        });
    }

    match peek_root_name(trimmed) {
        Some("open") => parse_stream_open(trimmed),
        Some("close") => parse_stream_close(trimmed),
        Some("iq" | "message" | "presence") => {
            parse_element_frame(&inject_client_ns_if_missing(trimmed))
        }
        Some(_) => parse_element_frame(trimmed),
        None => Err(ClientError::InvalidTransportFrame),
    }
}

fn parse_stream_open(frame: &str) -> ClientResult<TransportMessage> {
    let element = Element::from_str(frame).map_err(|_| ClientError::InvalidTransportFrame)?;
    if element.name() != "open" || element.ns() != NS_XMPP_FRAMING {
        return Err(ClientError::InvalidTransportFrame);
    }

    let version = element.attr("version").unwrap_or(StreamOpen::VERSION);
    if version != StreamOpen::VERSION {
        return Err(ClientError::UnsupportedStreamVersion {
            version: version.to_string(),
        });
    }

    let to = element
        .attr("to")
        .map(BareJid::from_str)
        .transpose()
        .map_err(|_| ClientError::InvalidStreamOpenTo)?;
    let from = element
        .attr("from")
        .map(BareJid::from_str)
        .transpose()
        .map_err(|_| ClientError::InvalidStreamOpenFrom)?;

    Ok(TransportMessage::Open(StreamOpen {
        to,
        from,
        id: element.attr("id").map(StreamId::new),
        language: element.attr("xml:lang").map(str::to_string),
        version: StreamOpen::VERSION,
    }))
}

fn parse_stream_close(frame: &str) -> ClientResult<TransportMessage> {
    let element = Element::from_str(frame).map_err(|_| ClientError::InvalidTransportFrame)?;
    if element.name() != "close" || element.ns() != NS_XMPP_FRAMING {
        return Err(ClientError::InvalidTransportFrame);
    }

    Ok(TransportMessage::Close(StreamClose))
}

fn parse_element_frame(frame: &str) -> ClientResult<TransportMessage> {
    Element::from_str(frame)
        .map(TransportMessage::Element)
        .map_err(|_| ClientError::InvalidTransportFrame)
}

fn peek_root_name(xml: &str) -> Option<&str> {
    let rest = xml.strip_prefix('<')?;
    if rest.starts_with('?') || rest.starts_with('!') {
        return None;
    }

    let name_end = rest
        .find(|c: char| c.is_ascii_whitespace() || c == '>' || c == '/')
        .unwrap_or(rest.len());
    if name_end == 0 {
        return None;
    }

    Some(&rest[..name_end])
}

fn inject_client_ns_if_missing(xml: &str) -> String {
    let trimmed = xml.trim();
    let Some(scan) = scan_start_tag(trimmed) else {
        return trimmed.to_string();
    };
    if scan.has_default_xmlns {
        return trimmed.to_string();
    }

    let mut patched = String::with_capacity(trimmed.len() + NS_CLIENT.len() + 9);
    patched.push_str(&trimmed[..scan.name_end]);
    patched.push_str(r#" xmlns=""#);
    patched.push_str(NS_CLIENT);
    patched.push('"');
    patched.push_str(&trimmed[scan.name_end..]);
    patched
}

#[derive(Debug, Clone, Copy)]
struct StartTagScan {
    name_end: usize,
    has_default_xmlns: bool,
}

fn scan_start_tag(xml: &str) -> Option<StartTagScan> {
    let bytes = xml.as_bytes();
    if bytes.first().copied()? != b'<' {
        return None;
    }

    let mut idx = 1;
    while idx < bytes.len() && bytes[idx].is_ascii_whitespace() {
        idx += 1;
    }
    if idx >= bytes.len() || matches!(bytes[idx], b'/' | b'!' | b'?') {
        return None;
    }

    let name_start = idx;
    while idx < bytes.len()
        && !bytes[idx].is_ascii_whitespace()
        && !matches!(bytes[idx], b'/' | b'>')
    {
        idx += 1;
    }
    if idx == name_start {
        return None;
    }
    let name_end = idx;

    let mut quote = None;
    let mut has_default_xmlns = false;
    while idx < bytes.len() {
        let byte = bytes[idx];
        match quote {
            Some(q) if byte == q => quote = None,
            Some(_) => {}
            None if byte == b'"' || byte == b'\'' => quote = Some(byte),
            None if byte == b'>' => break,
            None if byte == b'x' && xml[idx..].starts_with("xmlns") => {
                idx += "xmlns".len();
                if idx < bytes.len() && bytes[idx] == b'=' {
                    idx += 1;
                    if idx < bytes.len() && (bytes[idx] == b'"' || bytes[idx] == b'\'') {
                        let q = bytes[idx];
                        // Only count as a non-empty default namespace when the value
                        // between the quotes is non-empty (i.e. not xmlns="").
                        if idx + 1 < bytes.len() && bytes[idx + 1] != q {
                            has_default_xmlns = true;
                        }
                        quote = Some(q);
                    } else if idx < bytes.len() && bytes[idx] != b'>' {
                        // Unquoted, non-empty value.
                        has_default_xmlns = true;
                    }
                    continue;
                }
            }
            None => {}
        }
        idx += 1;
    }

    Some(StartTagScan {
        name_end,
        has_default_xmlns,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::str::FromStr;

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    use crate::config::{AccessToken, ClientResource, OAuthBearerConfig, WebSocketConfig};
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    use crate::ConnectionConfig;
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    use futures::{SinkExt, StreamExt};
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    use tokio::net::TcpListener;
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    use tokio_tungstenite::accept_hdr_async;
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    use tokio_tungstenite::tungstenite::handshake::server::{ErrorResponse, Request, Response};
    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    use url::Url;

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    fn config() -> ClientConfig {
        ClientConfig::new(
            ConnectionConfig::new(BareJid::from_str("waddle.example").unwrap()),
            WebSocketConfig::new(Url::parse("wss://chat.example.com/ws").unwrap()).unwrap(),
            OAuthBearerConfig::new(
                BareJid::from_str("alice@example.com").unwrap(),
                ClientResource::new("macbook").unwrap(),
                AccessToken::new("token"),
            )
            .unwrap(),
        )
        .unwrap()
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    #[expect(
        clippy::result_large_err,
        reason = "tungstenite fixes the handshake callback error type as a large ErrorResponse"
    )]
    fn assert_xmpp_subprotocol(
        request: &Request,
        mut response: Response,
    ) -> Result<Response, ErrorResponse> {
        assert_eq!(
            request
                .headers()
                .get("Sec-WebSocket-Protocol")
                .and_then(|value| value.to_str().ok()),
            Some("xmpp")
        );
        response
            .headers_mut()
            .insert("Sec-WebSocket-Protocol", "xmpp".parse().unwrap());
        Ok(response)
    }

    #[test]
    fn transport_event_maps_close_messages_to_closing() {
        assert_eq!(
            TransportEvent::MessageReceived(TransportMessage::Close(StreamClose)).transport_state(),
            TransportState::Closing
        );
        assert_eq!(
            TransportEvent::MessageSent(TransportMessage::Close(StreamClose)).transport_state(),
            TransportState::Closing
        );
    }

    #[test]
    fn open_frames_round_trip_through_rfc7395_codec() {
        let message = TransportMessage::Open(StreamOpen {
            to: Some(BareJid::from_str("waddle.example").unwrap()),
            from: Some(BareJid::from_str("chat.example.com").unwrap()),
            id: Some(StreamId::new("stream-1")),
            language: Some("en".to_string()),
            version: StreamOpen::VERSION,
        });

        let encoded = encode_message(&message).unwrap();
        let decoded = decode_message(&encoded).unwrap();

        assert_eq!(decoded, message);
    }

    #[test]
    fn decode_injects_client_namespace_for_stanzas() {
        let message =
            decode_message(r#"<iq type="get" id="ping-1"><ping xmlns="urn:xmpp:ping"/></iq>"#)
                .unwrap();

        let TransportMessage::Element(element) = message else {
            panic!("expected element transport message");
        };

        assert_eq!(element.name(), "iq");
        assert_eq!(element.ns(), NS_CLIENT);
    }

    #[cfg(all(feature = "native", not(target_arch = "wasm32")))]
    #[tokio::test(flavor = "current_thread")]
    async fn concrete_transport_connects_and_emits_typed_frames() {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();

        let server = tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut websocket = accept_hdr_async(stream, assert_xmpp_subprotocol)
                .await
                .unwrap();

            let outbound = websocket.next().await.unwrap().unwrap();
            let Message::Text(text) = outbound else {
                panic!("expected text frame from client");
            };
            assert!(matches!(
                decode_message(text.as_ref()).unwrap(),
                TransportMessage::Open(_)
            ));

            websocket
                .send(
                    Message::Text(
                        r#"<open xmlns="urn:ietf:params:xml:ns:xmpp-framing" from="waddle.example" id="stream-1" version="1.0"/>"#
                            .to_string(),
                    ),
                )
                .await
                .unwrap();
            websocket
                .send(Message::Text(
                    r#"<iq type="result" id="ping-1"><ping xmlns="urn:xmpp:ping"/></iq>"#
                        .to_string(),
                ))
                .await
                .unwrap();
            websocket
                .send(Message::Text(
                    r#"<close xmlns="urn:ietf:params:xml:ns:xmpp-framing"/>"#.to_string(),
                ))
                .await
                .unwrap();
            websocket.close(None).await.unwrap();
        });

        let mut live_config = config();
        live_config.transport =
            WebSocketConfig::new(Url::parse(&format!("ws://{address}/xmpp")).unwrap()).unwrap();

        let factory = DefaultTransportFactory;
        let mut transport = factory.connect(&live_config).await.unwrap();

        assert_eq!(
            transport.next_event().await.unwrap(),
            Some(TransportEvent::StateChanged(TransportState::Connecting))
        );
        assert_eq!(
            transport.next_event().await.unwrap(),
            Some(TransportEvent::StateChanged(TransportState::Open))
        );

        let outbound = TransportMessage::Open(StreamOpen::from_config(&live_config));
        transport.send(outbound.clone()).await.unwrap();
        assert!(matches!(
            transport.next_event().await.unwrap(),
            Some(TransportEvent::MessageSent(message)) if message == outbound
        ));

        assert!(matches!(
            transport.next_event().await.unwrap(),
            Some(TransportEvent::MessageReceived(TransportMessage::Open(open)))
                if open.id.as_ref().map(StreamId::as_str) == Some("stream-1")
        ));
        assert!(matches!(
            transport.next_event().await.unwrap(),
            Some(TransportEvent::MessageReceived(TransportMessage::Element(element)))
                if element.name() == "iq" && element.ns() == NS_CLIENT
        ));
        assert_eq!(
            transport.next_event().await.unwrap(),
            Some(TransportEvent::StateChanged(TransportState::Closing))
        );
        assert_eq!(
            transport.next_event().await.unwrap(),
            Some(TransportEvent::MessageReceived(TransportMessage::Close(
                StreamClose
            )))
        );
        assert_eq!(
            transport.next_event().await.unwrap(),
            Some(TransportEvent::StateChanged(TransportState::Closed))
        );
        assert_eq!(
            transport.next_event().await.unwrap(),
            Some(TransportEvent::Closed)
        );

        server.await.unwrap();
    }
}
