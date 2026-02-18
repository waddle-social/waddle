use crate::error::ConnectionError;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConnectionConfig {
    pub jid: String,
    pub password: String,
    pub server: Option<String>,
    pub port: Option<u16>,
    pub timeout_seconds: u32,
    pub max_reconnect_attempts: u32,
}

/// Platform-abstracted XMPP transport.
///
/// Feature-gated implementations provide the concrete transport:
/// - `NativeTcpTransport` (native feature): TCP/TLS via tokio-xmpp + rustls
/// - `WebSocketTransport` (web feature): WebSocket via tokio-tungstenite / web-sys
pub trait XmppTransport: Send + 'static {
    fn connect(config: &ConnectionConfig) -> impl Future<Output = Result<Self, ConnectionError>>
    where
        Self: Sized;

    fn send(&mut self, data: &[u8]) -> impl Future<Output = Result<(), ConnectionError>>;

    fn recv(&mut self) -> impl Future<Output = Result<Vec<u8>, ConnectionError>>;

    fn close(&mut self) -> impl Future<Output = Result<(), ConnectionError>>;

    fn supports_stream_management(&self) -> bool;
}

#[cfg(feature = "native")]
mod native {
    use super::*;
    use bytes::BytesMut;
    use std::{
        sync::atomic::{AtomicBool, Ordering},
        time::Duration,
    };
    use tokio::{
        io::{AsyncReadExt, AsyncWriteExt},
        time::timeout,
    };
    use tokio_util::codec::Decoder;
    use tokio_xmpp::{
        connect::{AsyncReadAndWrite, ServerConnector},
        parsers::{jid::Jid, ns},
        starttls::{ServerConfig, error::Error as StartTlsError},
        tcp::{TcpServerConnector, error::Error as TcpConnectError},
        xmpp_stream::XMPPStream,
        Packet, XmppCodec,
    };
    use tracing::warn;

    const DEFAULT_XMPP_PORT: u16 = 5222;
    const INSECURE_TCP_ENV: &str = "WADDLE_XMPP_INSECURE_TCP";
    const MIN_TIMEOUT_SECONDS: u64 = 1;
    const RECV_BUFFER_SIZE: usize = 16 * 1024;
    const STREAM_PRIME: &str =
        "<stream:stream xmlns='jabber:client' xmlns:stream='http://etherx.jabber.org/streams'>";
    static LOOPBACK_TLS_FAILED: AtomicBool = AtomicBool::new(false);

    pub struct NativeTcpTransport {
        stream: Box<dyn AsyncReadAndWrite>,
        io_timeout: Duration,
        stream_management_supported: bool,
        inbound_codec: XmppCodec,
        inbound_buffer: BytesMut,
    }

    fn connect_timeout(config: &ConnectionConfig) -> Duration {
        Duration::from_secs(u64::from(config.timeout_seconds).max(MIN_TIMEOUT_SECONDS))
    }

    fn parse_jid(jid: &str) -> Result<Jid, ConnectionError> {
        jid.parse::<Jid>().map_err(|error| {
            ConnectionError::TransportError(format!("invalid JID '{jid}' in config: {error}"))
        })
    }

    fn to_server_config(config: &ConnectionConfig) -> ServerConfig {
        match &config.server {
            Some(host) => ServerConfig::Manual {
                host: host.clone(),
                port: config.port.unwrap_or(DEFAULT_XMPP_PORT),
            },
            None => ServerConfig::UseSrv,
        }
    }

    fn map_starttls_error(error: StartTlsError) -> ConnectionError {
        let message = error.to_string();
        let lower = message.to_ascii_lowercase();
        if lower.contains("dns")
            || lower.contains("resolve")
            || lower.contains("srv")
            || lower.contains("idna")
        {
            ConnectionError::DnsResolutionFailed(message)
        } else if lower.contains("tls")
            || lower.contains("certificate")
            || lower.contains("handshake")
            || lower.contains("no tls")
        {
            ConnectionError::TlsHandshakeFailed(message)
        } else {
            ConnectionError::TransportError(message)
        }
    }

    fn map_tcp_error(error: TcpConnectError) -> ConnectionError {
        let message = error.to_string();
        let lower = message.to_ascii_lowercase();
        if lower.contains("dns") || lower.contains("resolve") || lower.contains("srv") {
            ConnectionError::DnsResolutionFailed(message)
        } else {
            ConnectionError::TransportError(message)
        }
    }

    fn parse_bool_flag(value: &str) -> bool {
        let normalized = value.trim().to_ascii_lowercase();
        !normalized.is_empty()
            && normalized != "0"
            && normalized != "false"
            && normalized != "no"
            && normalized != "off"
    }

    fn insecure_tcp_env_override() -> Option<bool> {
        std::env::var(INSECURE_TCP_ENV)
            .ok()
            .map(|value| parse_bool_flag(&value))
    }

    fn is_loopback_host(host: &str) -> bool {
        let host = host.trim();
        let host = host
            .split_once("://")
            .map(|(_, remainder)| remainder)
            .unwrap_or(host);
        let host = host.split('/').next().unwrap_or(host);

        let host_without_port = if host.starts_with('[') {
            host.find(']').map(|end| &host[1..end]).unwrap_or(host)
        } else if host.matches(':').count() == 1 {
            host.split_once(':').map(|(name, _)| name).unwrap_or(host)
        } else {
            host
        };

        if let Ok(address) = host_without_port.parse::<std::net::IpAddr>() {
            return address.is_loopback();
        }

        let normalized = host_without_port.trim_end_matches('.').to_ascii_lowercase();
        normalized == "localhost" || normalized.ends_with(".localhost")
    }

    fn is_local_loopback_target(config: &ConnectionConfig, jid: &Jid) -> bool {
        let host = config
            .server
            .clone()
            .unwrap_or_else(|| jid.domain().to_string());
        is_loopback_host(&host)
    }

    fn insecure_tcp_target(config: &ConnectionConfig, jid: &Jid) -> String {
        let host = config
            .server
            .clone()
            .unwrap_or_else(|| jid.domain().to_string());
        let port = config.port.unwrap_or(DEFAULT_XMPP_PORT);
        format!("{host}:{port}")
    }

    fn map_io_error(error: std::io::Error) -> ConnectionError {
        ConnectionError::TransportError(error.to_string())
    }

    fn map_authentication_error(error: ConnectionError) -> ConnectionError {
        match error {
            ConnectionError::AuthenticationFailed(_) => error,
            other => ConnectionError::StreamError(format!("SASL negotiation failed: {other}")),
        }
    }

    fn prime_inbound_codec() -> XmppCodec {
        let mut codec = XmppCodec::new();
        let mut bootstrap = BytesMut::from(STREAM_PRIME.as_bytes());
        let _ = codec.decode(&mut bootstrap);
        codec
    }

    fn serialize_packet(packet: Packet) -> Result<Option<Vec<u8>>, ConnectionError> {
        match packet {
            Packet::Stanza(element) => {
                let mut payload = Vec::new();
                element
                    .write_to(&mut payload)
                    .map_err(|error| ConnectionError::TransportError(error.to_string()))?;
                Ok(Some(payload))
            }
            Packet::Text(_) => Ok(None),
            Packet::StreamStart(_) => Ok(None),
            Packet::StreamEnd => Err(ConnectionError::TransportError(
                "XMPP transport closed by peer".to_string(),
            )),
        }
    }

    async fn authenticate_stream<S>(
        xmpp_stream: XMPPStream<S>,
        username: &str,
        password: &str,
        io_timeout: Duration,
    ) -> Result<(Box<dyn AsyncReadAndWrite>, bool), ConnectionError>
    where
        S: AsyncReadAndWrite + 'static,
    {
        let authenticated = timeout(
            io_timeout,
            crate::sasl::authenticate(xmpp_stream, username, password),
        )
        .await
        .map_err(|_| ConnectionError::Timeout)?
        .map_err(map_authentication_error)?;

        Ok((
            Box::new(authenticated.stream),
            authenticated.stream_management_supported,
        ))
    }

    async fn connect_via_starttls(
        config: &ConnectionConfig,
        jid: &Jid,
        username: &str,
        io_timeout: Duration,
    ) -> Result<(Box<dyn AsyncReadAndWrite>, bool), ConnectionError> {
        let server_config = to_server_config(config);
        let xmpp_stream = timeout(io_timeout, server_config.connect(jid, ns::JABBER_CLIENT))
            .await
            .map_err(|_| ConnectionError::Timeout)?
            .map_err(map_starttls_error)?;

        authenticate_stream(xmpp_stream, username, &config.password, io_timeout).await
    }

    async fn connect_via_insecure_tcp(
        config: &ConnectionConfig,
        jid: &Jid,
        username: &str,
        io_timeout: Duration,
    ) -> Result<(Box<dyn AsyncReadAndWrite>, bool), ConnectionError> {
        let address = insecure_tcp_target(config, jid);
        let connector = TcpServerConnector::new(address);
        let xmpp_stream = timeout(io_timeout, connector.connect(jid, ns::JABBER_CLIENT))
            .await
            .map_err(|_| ConnectionError::Timeout)?
            .map_err(map_tcp_error)?;

        authenticate_stream(xmpp_stream, username, &config.password, io_timeout).await
    }

    impl XmppTransport for NativeTcpTransport {
        async fn connect(config: &ConnectionConfig) -> Result<Self, ConnectionError> {
            let jid = parse_jid(&config.jid)?;
            let io_timeout = connect_timeout(config);

            let username = jid.node().ok_or_else(|| {
                ConnectionError::AuthenticationFailed(format!(
                    "JID '{}' has no local part for SASL authentication",
                    config.jid
                ))
            })?;

            let insecure_override = insecure_tcp_env_override();
            let loopback_target = is_local_loopback_target(config, &jid);
            let prefer_insecure = insecure_override == Some(true)
                || (insecure_override.is_none()
                    && loopback_target
                    && LOOPBACK_TLS_FAILED.load(Ordering::Relaxed));

            let (stream, stream_management_supported): (Box<dyn AsyncReadAndWrite>, bool) =
                if prefer_insecure {
                    connect_via_insecure_tcp(config, &jid, username.as_str(), io_timeout).await?
                } else {
                    match connect_via_starttls(config, &jid, username.as_str(), io_timeout).await {
                        Ok(result) => {
                            if loopback_target {
                                LOOPBACK_TLS_FAILED.store(false, Ordering::Relaxed);
                            }
                            result
                        }
                        Err(error)
                            if insecure_override.is_none()
                                && loopback_target
                                && matches!(error, ConnectionError::TlsHandshakeFailed(_)) =>
                        {
                            LOOPBACK_TLS_FAILED.store(true, Ordering::Relaxed);
                            warn!(
                                reason = %error,
                                env = INSECURE_TCP_ENV,
                                "TLS failed against loopback target; retrying with insecure TCP"
                            );
                            connect_via_insecure_tcp(config, &jid, username.as_str(), io_timeout)
                                .await?
                        }
                        Err(error) => return Err(error),
                    }
                };

            Ok(Self {
                stream,
                io_timeout,
                stream_management_supported,
                inbound_codec: prime_inbound_codec(),
                inbound_buffer: BytesMut::with_capacity(RECV_BUFFER_SIZE),
            })
        }

        async fn send(&mut self, data: &[u8]) -> Result<(), ConnectionError> {
            if data.is_empty() {
                return Ok(());
            }

            timeout(self.io_timeout, self.stream.write_all(data))
                .await
                .map_err(|_| ConnectionError::Timeout)?
                .map_err(map_io_error)?;

            timeout(self.io_timeout, self.stream.flush())
                .await
                .map_err(|_| ConnectionError::Timeout)?
                .map_err(map_io_error)?;

            Ok(())
        }

        async fn recv(&mut self) -> Result<Vec<u8>, ConnectionError> {
            loop {
                if let Some(packet) = self
                    .inbound_codec
                    .decode(&mut self.inbound_buffer)
                    .map_err(|error| ConnectionError::TransportError(error.to_string()))?
                {
                    if let Some(payload) = serialize_packet(packet)? {
                        return Ok(payload);
                    }
                }

                let mut chunk = vec![0_u8; RECV_BUFFER_SIZE];
                let bytes_read = timeout(self.io_timeout, self.stream.read(&mut chunk))
                    .await
                    .map_err(|_| ConnectionError::Timeout)?
                    .map_err(map_io_error)?;

                if bytes_read == 0 {
                    return Err(ConnectionError::TransportError(
                        "XMPP transport closed by peer".to_string(),
                    ));
                }

                self.inbound_buffer.extend_from_slice(&chunk[..bytes_read]);
            }
        }

        async fn close(&mut self) -> Result<(), ConnectionError> {
            timeout(self.io_timeout, self.stream.shutdown())
                .await
                .map_err(|_| ConnectionError::Timeout)?
                .map_err(map_io_error)?;
            Ok(())
        }

        fn supports_stream_management(&self) -> bool {
            self.stream_management_supported
        }
    }
}

#[cfg(feature = "web")]
mod web {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;
    use xmpp_parsers::jid::Jid;

    const DEFAULT_WEBSOCKET_PORT: u16 = 443;
    const MIN_TIMEOUT_SECONDS: u64 = 1;
    #[cfg(any(test, target_arch = "wasm32"))]
    const XEP_0156_WEBSOCKET_REL: &str = "urn:xmpp:alt-connections:websocket";

    fn connect_timeout(config: &ConnectionConfig) -> Duration {
        Duration::from_secs(u64::from(config.timeout_seconds).max(MIN_TIMEOUT_SECONDS))
    }

    fn jid_domain(jid: &str) -> Result<String, ConnectionError> {
        jid.parse::<Jid>()
            .map(|parsed| parsed.domain().to_string())
            .map_err(|error| {
                ConnectionError::TransportError(format!("invalid JID '{jid}' in config: {error}"))
            })
    }

    fn server_to_websocket_url(server: &str, default_port: u16) -> Result<String, ConnectionError> {
        if server.starts_with("ws://") || server.starts_with("wss://") {
            return Ok(server.to_string());
        }

        if server.contains("://") {
            return Err(ConnectionError::TransportError(format!(
                "unsupported WebSocket scheme for server '{server}'"
            )));
        }

        let host_or_path = server.trim_matches('/');
        if host_or_path.is_empty() {
            return Err(ConnectionError::TransportError(
                "server value cannot be empty".to_string(),
            ));
        }

        if host_or_path.contains('/') {
            return Ok(format!("wss://{host_or_path}"));
        }

        let has_explicit_port = host_or_path
            .rsplit_once(':')
            .map(|(_, suffix)| suffix.chars().all(|character| character.is_ascii_digit()))
            .unwrap_or(false);

        if has_explicit_port {
            Ok(format!("wss://{host_or_path}/xmpp-websocket"))
        } else {
            Ok(format!(
                "wss://{host_or_path}:{default_port}/xmpp-websocket"
            ))
        }
    }

    #[cfg(any(test, target_arch = "wasm32"))]
    fn extract_xml_attribute(tag: &str, attribute: &str) -> Option<String> {
        ['"', '\''].into_iter().find_map(|quote| {
            let marker = format!("{attribute}={quote}");
            tag.find(&marker).and_then(|start| {
                let value_start = start + marker.len();
                let remainder = &tag[value_start..];
                remainder
                    .find(quote)
                    .map(|end| remainder[..end].to_string())
            })
        })
    }

    #[cfg(any(test, target_arch = "wasm32"))]
    fn parse_host_meta_websocket_endpoint(host_meta: &str) -> Option<String> {
        host_meta.split('<').skip(1).find_map(|segment| {
            let trimmed = segment.trim_start();
            let lower = trimmed.to_ascii_lowercase();
            if !lower.starts_with("link") {
                return None;
            }

            let rel = extract_xml_attribute(trimmed, "rel")?;
            if rel != XEP_0156_WEBSOCKET_REL {
                return None;
            }

            extract_xml_attribute(trimmed, "href")
        })
    }

    #[cfg(target_arch = "wasm32")]
    async fn discover_xep0156_endpoint(domain: &str) -> Result<Option<String>, ConnectionError> {
        use wasm_bindgen::JsCast;
        use wasm_bindgen_futures::JsFuture;
        use web_sys::Response;

        let window = web_sys::window().ok_or_else(|| {
            ConnectionError::TransportError("browser window is not available".to_string())
        })?;
        let host_meta_url = format!("https://{domain}/.well-known/host-meta");

        let response_value = JsFuture::from(window.fetch_with_str(&host_meta_url))
            .await
            .map_err(|error| {
                ConnectionError::TransportError(format!(
                    "failed to fetch XEP-0156 host-meta from '{host_meta_url}': {error:?}"
                ))
            })?;
        let response: Response = response_value.dyn_into().map_err(|_| {
            ConnectionError::TransportError("unexpected host-meta response type".to_string())
        })?;

        if !response.ok() {
            return Ok(None);
        }

        let text_promise = response.text().map_err(|error| {
            ConnectionError::TransportError(format!(
                "failed to read XEP-0156 host-meta response text: {error:?}"
            ))
        })?;
        let text_value = JsFuture::from(text_promise).await.map_err(|error| {
            ConnectionError::TransportError(format!(
                "failed awaiting XEP-0156 host-meta response body: {error:?}"
            ))
        })?;
        let host_meta = text_value.as_string().ok_or_else(|| {
            ConnectionError::TransportError(
                "XEP-0156 host-meta response body was not text".to_string(),
            )
        })?;

        Ok(parse_host_meta_websocket_endpoint(&host_meta))
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn discover_xep0156_endpoint(_domain: &str) -> Result<Option<String>, ConnectionError> {
        Ok(None)
    }

    async fn resolve_websocket_url(config: &ConnectionConfig) -> Result<String, ConnectionError> {
        let default_port = config.port.unwrap_or(DEFAULT_WEBSOCKET_PORT);
        if let Some(server) = config.server.as_deref() {
            return server_to_websocket_url(server, default_port);
        }

        let domain = jid_domain(&config.jid)?;
        if let Some(discovered_url) = discover_xep0156_endpoint(&domain).await? {
            return Ok(discovered_url);
        }

        Ok(format!("wss://{domain}:{default_port}/xmpp-websocket"))
    }

    #[cfg(not(target_arch = "wasm32"))]
    type BlockingWebSocket = tokio_tungstenite::tungstenite::WebSocket<
        tokio_tungstenite::tungstenite::stream::MaybeTlsStream<std::net::TcpStream>,
    >;

    #[cfg(not(target_arch = "wasm32"))]
    fn map_websocket_error(error: tokio_tungstenite::tungstenite::Error) -> ConnectionError {
        let message = error.to_string();
        let lower = message.to_ascii_lowercase();
        if lower.contains("dns")
            || lower.contains("resolve")
            || lower.contains("unable to connect")
            || lower.contains("failed to lookup")
        {
            ConnectionError::DnsResolutionFailed(message)
        } else if lower.contains("tls")
            || lower.contains("certificate")
            || lower.contains("handshake")
            || lower.contains("tlsfeaturenotenabled")
        {
            ConnectionError::TlsHandshakeFailed(message)
        } else {
            ConnectionError::TransportError(message)
        }
    }

    #[cfg(not(target_arch = "wasm32"))]
    async fn run_blocking_with_timeout<R, F>(
        io_timeout: Duration,
        operation: F,
    ) -> Result<R, ConnectionError>
    where
        R: Send + 'static,
        F: FnOnce() -> Result<R, ConnectionError> + Send + 'static,
    {
        timeout(io_timeout, tokio::task::spawn_blocking(operation))
            .await
            .map_err(|_| ConnectionError::Timeout)?
            .map_err(|error| ConnectionError::TransportError(error.to_string()))?
    }

    #[cfg(not(target_arch = "wasm32"))]
    pub struct WebSocketTransport {
        socket: std::sync::Arc<std::sync::Mutex<BlockingWebSocket>>,
        io_timeout: Duration,
    }

    #[cfg(not(target_arch = "wasm32"))]
    impl XmppTransport for WebSocketTransport {
        async fn connect(config: &ConnectionConfig) -> Result<Self, ConnectionError> {
            let url = resolve_websocket_url(config).await?;
            let io_timeout = connect_timeout(config);

            let websocket = run_blocking_with_timeout(io_timeout, move || {
                tokio_tungstenite::tungstenite::connect(url.as_str())
                    .map(|(socket, _response)| socket)
                    .map_err(map_websocket_error)
            })
            .await?;

            Ok(Self {
                socket: std::sync::Arc::new(std::sync::Mutex::new(websocket)),
                io_timeout,
            })
        }

        async fn send(&mut self, data: &[u8]) -> Result<(), ConnectionError> {
            if data.is_empty() {
                return Ok(());
            }

            let text = std::str::from_utf8(data).map_err(|error| {
                ConnectionError::TransportError(format!(
                    "RFC 7395 requires UTF-8 text frames; invalid payload: {error}"
                ))
            })?;
            let message = tokio_tungstenite::tungstenite::Message::Text(text.to_string().into());
            let socket = std::sync::Arc::clone(&self.socket);

            run_blocking_with_timeout(self.io_timeout, move || {
                let mut websocket = socket.lock().map_err(|_| {
                    ConnectionError::TransportError("websocket state mutex poisoned".to_string())
                })?;
                websocket.send(message).map_err(map_websocket_error)
            })
            .await
        }

        async fn recv(&mut self) -> Result<Vec<u8>, ConnectionError> {
            let socket = std::sync::Arc::clone(&self.socket);

            run_blocking_with_timeout(self.io_timeout, move || {
                loop {
                    let message = {
                        let mut websocket = socket.lock().map_err(|_| {
                            ConnectionError::TransportError(
                                "websocket state mutex poisoned".to_string(),
                            )
                        })?;
                        websocket.read().map_err(map_websocket_error)?
                    };

                    match message {
                        tokio_tungstenite::tungstenite::Message::Text(text) => {
                            return Ok(text.to_string().into_bytes());
                        }
                        tokio_tungstenite::tungstenite::Message::Binary(bytes) => {
                            return Ok(bytes.to_vec());
                        }
                        tokio_tungstenite::tungstenite::Message::Close(_) => {
                            return Err(ConnectionError::TransportError(
                                "websocket closed by peer".to_string(),
                            ));
                        }
                        tokio_tungstenite::tungstenite::Message::Ping(_)
                        | tokio_tungstenite::tungstenite::Message::Pong(_)
                        | tokio_tungstenite::tungstenite::Message::Frame(_) => {}
                    }
                }
            })
            .await
        }

        async fn close(&mut self) -> Result<(), ConnectionError> {
            let socket = std::sync::Arc::clone(&self.socket);

            run_blocking_with_timeout(self.io_timeout, move || {
                let mut websocket = socket.lock().map_err(|_| {
                    ConnectionError::TransportError("websocket state mutex poisoned".to_string())
                })?;
                websocket.close(None).map_err(map_websocket_error)
            })
            .await
        }

        fn supports_stream_management(&self) -> bool {
            false
        }
    }

    #[cfg(target_arch = "wasm32")]
    enum WebSocketCommand {
        Send(String),
        Close,
    }

    #[cfg(target_arch = "wasm32")]
    pub struct WebSocketTransport {
        outbound: tokio::sync::mpsc::UnboundedSender<WebSocketCommand>,
        inbound: tokio::sync::mpsc::UnboundedReceiver<Result<Vec<u8>, ConnectionError>>,
    }

    #[cfg(target_arch = "wasm32")]
    async fn run_browser_websocket(
        socket: web_sys::WebSocket,
        mut outbound: tokio::sync::mpsc::UnboundedReceiver<WebSocketCommand>,
        inbound: tokio::sync::mpsc::UnboundedSender<Result<Vec<u8>, ConnectionError>>,
    ) {
        use wasm_bindgen::{JsCast, closure::Closure};
        use web_sys::{CloseEvent, ErrorEvent, MessageEvent};

        let inbound_for_message = inbound.clone();
        let onmessage = Closure::wrap(Box::new(move |event: MessageEvent| {
            if let Some(text) = event.data().as_string() {
                let _ = inbound_for_message.send(Ok(text.into_bytes()));
                return;
            }

            let _ = inbound_for_message.send(Err(ConnectionError::TransportError(
                "received non-text WebSocket frame".to_string(),
            )));
        }) as Box<dyn FnMut(MessageEvent)>);
        socket.set_onmessage(Some(onmessage.as_ref().unchecked_ref()));
        onmessage.forget();

        let inbound_for_error = inbound.clone();
        let onerror = Closure::wrap(Box::new(move |event: ErrorEvent| {
            let _ = inbound_for_error.send(Err(ConnectionError::TransportError(format!(
                "websocket error: {}",
                event.message()
            ))));
        }) as Box<dyn FnMut(ErrorEvent)>);
        socket.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onerror.forget();

        let inbound_for_close = inbound.clone();
        let onclose = Closure::wrap(Box::new(move |event: CloseEvent| {
            let _ = inbound_for_close.send(Err(ConnectionError::TransportError(format!(
                "websocket closed: code={} reason={}",
                event.code(),
                event.reason()
            ))));
        }) as Box<dyn FnMut(CloseEvent)>);
        socket.set_onclose(Some(onclose.as_ref().unchecked_ref()));
        onclose.forget();

        while let Some(command) = outbound.recv().await {
            match command {
                WebSocketCommand::Send(message) => {
                    if socket.ready_state() != web_sys::WebSocket::OPEN {
                        let _ = inbound.send(Err(ConnectionError::TransportError(
                            "websocket is not open".to_string(),
                        )));
                        continue;
                    }
                    if let Err(error) = socket.send_with_str(&message) {
                        let _ = inbound.send(Err(ConnectionError::TransportError(format!(
                            "failed to send websocket frame: {error:?}"
                        ))));
                    }
                }
                WebSocketCommand::Close => {
                    let _ = socket.close();
                    break;
                }
            }
        }
    }

    #[cfg(target_arch = "wasm32")]
    impl XmppTransport for WebSocketTransport {
        async fn connect(config: &ConnectionConfig) -> Result<Self, ConnectionError> {
            let url = resolve_websocket_url(config).await?;
            let socket = web_sys::WebSocket::new(url.as_str()).map_err(|error| {
                ConnectionError::TransportError(format!(
                    "failed to open browser websocket connection: {error:?}"
                ))
            })?;

            let (outbound_tx, outbound_rx) = tokio::sync::mpsc::unbounded_channel();
            let (inbound_tx, inbound_rx) = tokio::sync::mpsc::unbounded_channel();

            wasm_bindgen_futures::spawn_local(run_browser_websocket(
                socket,
                outbound_rx,
                inbound_tx,
            ));

            Ok(Self {
                outbound: outbound_tx,
                inbound: inbound_rx,
            })
        }

        async fn send(&mut self, data: &[u8]) -> Result<(), ConnectionError> {
            let text = std::str::from_utf8(data).map_err(|error| {
                ConnectionError::TransportError(format!(
                    "RFC 7395 requires UTF-8 text frames; invalid payload: {error}"
                ))
            })?;
            self.outbound
                .send(WebSocketCommand::Send(text.to_string()))
                .map_err(|error| ConnectionError::TransportError(error.to_string()))
        }

        async fn recv(&mut self) -> Result<Vec<u8>, ConnectionError> {
            self.inbound.recv().await.unwrap_or_else(|| {
                Err(ConnectionError::TransportError(
                    "websocket receive channel closed".to_string(),
                ))
            })
        }

        async fn close(&mut self) -> Result<(), ConnectionError> {
            self.outbound
                .send(WebSocketCommand::Close)
                .map_err(|error| ConnectionError::TransportError(error.to_string()))?;
            Ok(())
        }

        fn supports_stream_management(&self) -> bool {
            false
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn server_without_scheme_uses_default_websocket_path() {
            let url = server_to_websocket_url("chat.example.com", 443).unwrap();
            assert_eq!(url, "wss://chat.example.com:443/xmpp-websocket");
        }

        #[test]
        fn server_with_scheme_is_used_verbatim() {
            let url = server_to_websocket_url("wss://xmpp.example.com/ws", 443).unwrap();
            assert_eq!(url, "wss://xmpp.example.com/ws");
        }

        #[test]
        fn xep0156_host_meta_parser_finds_websocket_link() {
            let host_meta = r#"<?xml version='1.0'?>
<XRD xmlns='http://docs.oasis-open.org/ns/xri/xrd-1.0'>
    <Link rel='urn:xmpp:alt-connections:websocket' href='wss://xmpp.example.com/ws'/>
</XRD>"#;

            let discovered = parse_host_meta_websocket_endpoint(host_meta);
            assert_eq!(discovered, Some("wss://xmpp.example.com/ws".to_string()));
        }
    }
}

#[cfg(feature = "native")]
pub use native::NativeTcpTransport;

#[cfg(feature = "web")]
pub use web::WebSocketTransport;
