//! S2S Outbound Connection for initiating connections to remote servers.
//!
//! This module handles outbound federation connections including:
//! - DNS SRV resolution for target discovery
//! - TCP connection establishment
//! - TLS negotiation (STARTTLS)
//! - Stream negotiation (XML stream headers and features)
//! - Server Dialback authentication (XEP-0220)
//!
//! # Connection Flow
//!
//! 1. DNS resolution: Resolve `_xmpp-server._tcp.{domain}` SRV records
//! 2. TCP connect: Establish TCP connection to resolved target
//! 3. Send stream header: `<stream:stream xmlns='jabber:server' ...>`
//! 4. Receive stream header and features (STARTTLS required)
//! 5. TLS upgrade: STARTTLS handshake
//! 6. Re-send stream header (post-TLS)
//! 7. Receive post-TLS features (dialback)
//! 8. Send dialback result: `<db:result from='local' to='remote'>key</db:result>`
//! 9. Receive dialback response: `<db:result type='valid'/>`
//! 10. Connection established, ready for stanza routing

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;
use tokio_rustls::TlsConnector;
use tracing::{debug, info, instrument, warn};

use crate::parser::{ns, ParsedStanza, StreamHeader, XmlParser};
use crate::s2s::dialback::{build_db_result, DialbackKey, DialbackState};
use crate::s2s::dns::{DnsError, ResolvedTarget, SrvResolver};

/// Errors that can occur during outbound S2S connection.
#[derive(Debug, Error)]
pub enum OutboundConnectionError {
    /// DNS resolution failed
    #[error("DNS resolution failed: {0}")]
    DnsResolution(#[from] DnsError),

    /// TCP connection failed
    #[error("TCP connection failed: {0}")]
    TcpConnect(#[source] std::io::Error),

    /// TLS handshake failed
    #[error("TLS handshake failed: {0}")]
    TlsHandshake(String),

    /// Stream negotiation failed
    #[error("Stream negotiation failed: {0}")]
    StreamNegotiation(String),

    /// Dialback authentication failed
    #[error("Dialback authentication failed: {0}")]
    DialbackFailed(String),

    /// Connection closed unexpectedly
    #[error("Connection closed unexpectedly")]
    ConnectionClosed,

    /// Protocol error
    #[error("Protocol error: {0}")]
    Protocol(String),

    /// IO error
    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    /// Configuration error
    #[error("Configuration error: {0}")]
    Config(String),
}

/// Configuration for outbound S2S connections.
#[derive(Clone)]
pub struct S2sOutboundConfig {
    /// TLS connector for secure connections.
    pub tls_connector: TlsConnector,

    /// Secret for dialback key generation (XEP-0220).
    pub dialback_secret: Vec<u8>,

    /// Whether to verify TLS certificates (disable for testing only).
    pub verify_certificates: bool,
}

impl std::fmt::Debug for S2sOutboundConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("S2sOutboundConfig")
            .field("dialback_secret", &"[REDACTED]")
            .field("verify_certificates", &self.verify_certificates)
            .finish()
    }
}

impl S2sOutboundConfig {
    /// Create a new outbound config with the given TLS connector and dialback secret.
    pub fn new(tls_connector: TlsConnector, dialback_secret: Vec<u8>) -> Self {
        Self {
            tls_connector,
            dialback_secret,
            verify_certificates: true,
        }
    }
}

/// State of an outbound S2S connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OutboundState {
    /// Initial state, not connected
    Disconnected,
    /// TCP connected, pre-TLS
    TcpConnected,
    /// TLS negotiated
    TlsConnected,
    /// Dialback in progress
    Dialback,
    /// Fully authenticated and ready
    Established,
    /// Connection closed
    Closed,
}

/// An outbound S2S connection to a remote XMPP server.
pub struct S2sOutboundConnection {
    /// The remote domain we're connected to.
    remote_domain: String,

    /// Our local domain.
    local_domain: String,

    /// Current connection state.
    state: OutboundState,

    /// The underlying TLS stream (None if not yet upgraded).
    stream: Option<OutboundStream>,

    /// XML parser for reading stanzas.
    parser: XmlParser,

    /// Dialback key generator.
    dialback_key: DialbackKey,

    /// Current stream ID (assigned by remote server).
    stream_id: Option<String>,

    /// Dialback state.
    dialback_state: DialbackState,

    /// When the connection was established.
    connected_at: Option<Instant>,

    /// Whether the connection is still alive.
    connected: Arc<AtomicBool>,
}

/// The underlying stream for outbound connections.
enum OutboundStream {
    /// Plain TCP stream (pre-STARTTLS).
    Tcp(TcpStream),
    /// TLS stream (post-STARTTLS).
    Tls(Box<TlsStream<TcpStream>>),
}

impl S2sOutboundConnection {
    /// Connect to a remote XMPP server.
    ///
    /// This performs the full connection sequence:
    /// 1. TCP connect
    /// 2. Stream negotiation (pre-TLS)
    /// 3. STARTTLS upgrade
    /// 4. Stream negotiation (post-TLS)
    /// 5. Dialback authentication
    #[instrument(
        name = "s2s.outbound.connect",
        skip(config),
        fields(
            remote_domain = %remote_domain,
            local_domain = %local_domain,
            target = %target_host
        )
    )]
    pub async fn connect(
        target_host: &str,
        target_port: u16,
        remote_domain: &str,
        local_domain: &str,
        config: S2sOutboundConfig,
    ) -> Result<Self, OutboundConnectionError> {
        info!(
            target = %target_host,
            port = target_port,
            "Initiating S2S outbound connection"
        );

        // Create the connection object
        let mut conn = Self {
            remote_domain: remote_domain.to_string(),
            local_domain: local_domain.to_string(),
            state: OutboundState::Disconnected,
            stream: None,
            parser: XmlParser::new(),
            dialback_key: DialbackKey::new(&config.dialback_secret),
            stream_id: None,
            dialback_state: DialbackState::None,
            connected_at: None,
            connected: Arc::new(AtomicBool::new(false)),
        };

        // Resolve target to socket addresses
        let resolver = SrvResolver::new().await?;
        let addrs = resolver
            .resolve_host_to_addrs(target_host, target_port)
            .await?;

        // Try to connect to each address
        let tcp_stream = conn.tcp_connect(&addrs).await?;
        conn.stream = Some(OutboundStream::Tcp(tcp_stream));
        conn.state = OutboundState::TcpConnected;

        // Perform stream negotiation (pre-TLS)
        conn.send_stream_header().await?;
        let header = conn.read_stream_header().await?;
        conn.stream_id = header.id;

        // Read features, expect STARTTLS
        conn.expect_starttls_feature().await?;

        // Perform STARTTLS upgrade
        conn.upgrade_to_tls(&config.tls_connector, target_host)
            .await?;
        conn.state = OutboundState::TlsConnected;

        // Perform stream negotiation (post-TLS)
        conn.parser.reset();
        conn.send_stream_header().await?;
        let header = conn.read_stream_header().await?;
        conn.stream_id = header.id;

        // Read post-TLS features, expect dialback
        conn.expect_dialback_feature().await?;

        // Perform dialback authentication
        conn.perform_dialback().await?;
        conn.state = OutboundState::Established;
        conn.connected_at = Some(Instant::now());
        conn.connected.store(true, Ordering::Relaxed);

        info!(
            remote_domain = %remote_domain,
            stream_id = ?conn.stream_id,
            "S2S outbound connection established"
        );

        Ok(conn)
    }

    /// Connect to a remote server using pre-resolved targets.
    #[instrument(
        name = "s2s.outbound.connect_targets",
        skip(config, targets),
        fields(
            remote_domain = %remote_domain,
            local_domain = %local_domain,
            target_count = targets.len()
        )
    )]
    pub async fn connect_with_targets(
        targets: Vec<ResolvedTarget>,
        remote_domain: &str,
        local_domain: &str,
        config: S2sOutboundConfig,
    ) -> Result<Self, OutboundConnectionError> {
        if targets.is_empty() {
            return Err(OutboundConnectionError::Config(
                "No targets provided".to_string(),
            ));
        }

        // Try each target in order
        let mut last_error = None;
        for target in targets {
            debug!(
                host = %target.host,
                port = target.port,
                priority = target.priority,
                "Trying S2S target"
            );

            match Self::connect(
                &target.host,
                target.port,
                remote_domain,
                local_domain,
                config.clone(),
            )
            .await
            {
                Ok(conn) => return Ok(conn),
                Err(e) => {
                    warn!(
                        host = %target.host,
                        error = %e,
                        "Failed to connect to S2S target"
                    );
                    last_error = Some(e);
                }
            }
        }

        Err(last_error
            .unwrap_or_else(|| OutboundConnectionError::Config("No targets available".to_string())))
    }

    /// Attempt TCP connection to the first available address.
    async fn tcp_connect(
        &self,
        addrs: &[SocketAddr],
    ) -> Result<TcpStream, OutboundConnectionError> {
        let mut last_error = None;

        for addr in addrs {
            debug!(addr = %addr, "Attempting TCP connection");
            match TcpStream::connect(addr).await {
                Ok(stream) => {
                    debug!(addr = %addr, "TCP connection established");
                    return Ok(stream);
                }
                Err(e) => {
                    warn!(addr = %addr, error = %e, "TCP connection failed");
                    last_error = Some(e);
                }
            }
        }

        Err(OutboundConnectionError::TcpConnect(
            last_error.unwrap_or_else(|| {
                std::io::Error::new(std::io::ErrorKind::NotConnected, "No addresses available")
            }),
        ))
    }

    /// Send the initial stream header.
    async fn send_stream_header(&mut self) -> Result<(), OutboundConnectionError> {
        let header = format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:server' \
            xmlns:stream='http://etherx.jabber.org/streams' \
            xmlns:db='jabber:server:dialback' \
            to='{}' from='{}' version='1.0'>",
            self.remote_domain, self.local_domain
        );

        self.write_all(header.as_bytes()).await?;
        self.flush().await?;

        debug!(
            to = %self.remote_domain,
            from = %self.local_domain,
            "Sent S2S stream header"
        );

        Ok(())
    }

    /// Read the stream header response.
    async fn read_stream_header(&mut self) -> Result<StreamHeader, OutboundConnectionError> {
        let mut buf = [0u8; 4096];

        loop {
            let n = self.read(&mut buf).await?;
            if n == 0 {
                return Err(OutboundConnectionError::ConnectionClosed);
            }

            self.parser.feed(&buf[..n]);

            if self.parser.has_stream_header() {
                break;
            }
        }

        let header = self
            .parser
            .take_stream_header()
            .map_err(|e| OutboundConnectionError::StreamNegotiation(e.to_string()))?;

        debug!(
            from = ?header.from,
            id = ?header.id,
            version = ?header.version,
            "Received S2S stream header"
        );

        Ok(header)
    }

    /// Wait for and validate STARTTLS feature.
    async fn expect_starttls_feature(&mut self) -> Result<(), OutboundConnectionError> {
        let mut buf = [0u8; 4096];

        loop {
            let n = self.read(&mut buf).await?;
            if n == 0 {
                return Err(OutboundConnectionError::ConnectionClosed);
            }

            self.parser.feed(&buf[..n]);

            if self.parser.has_complete_stanza() {
                if let Some(stanza) = self
                    .parser
                    .next_stanza()
                    .map_err(|e| OutboundConnectionError::StreamNegotiation(e.to_string()))?
                {
                    match stanza {
                        ParsedStanza::Features { starttls, .. } => {
                            if starttls {
                                debug!("Remote server advertises STARTTLS");
                                return Ok(());
                            } else {
                                return Err(OutboundConnectionError::StreamNegotiation(
                                    "Remote server does not offer STARTTLS".to_string(),
                                ));
                            }
                        }
                        _ => {
                            // Keep reading for features
                            continue;
                        }
                    }
                }
            }
        }
    }

    /// Upgrade the connection to TLS.
    async fn upgrade_to_tls(
        &mut self,
        tls_connector: &TlsConnector,
        server_name: &str,
    ) -> Result<(), OutboundConnectionError> {
        // Send STARTTLS request
        let starttls_request = format!("<starttls xmlns='{}'/>", ns::TLS);
        self.write_all(starttls_request.as_bytes()).await?;
        self.flush().await?;

        debug!("Sent STARTTLS request");

        // Wait for proceed
        let mut buf = [0u8; 1024];
        loop {
            let n = self.read(&mut buf).await?;
            if n == 0 {
                return Err(OutboundConnectionError::ConnectionClosed);
            }

            self.parser.feed(&buf[..n]);

            if self.parser.has_complete_stanza() {
                if let Some(stanza) = self
                    .parser
                    .next_stanza()
                    .map_err(|e| OutboundConnectionError::TlsHandshake(e.to_string()))?
                {
                    match stanza {
                        ParsedStanza::TlsProceed => {
                            debug!("Received TLS proceed");
                            break;
                        }
                        ParsedStanza::TlsFailure => {
                            return Err(OutboundConnectionError::TlsHandshake(
                                "Server rejected STARTTLS".to_string(),
                            ));
                        }
                        _ => {
                            return Err(OutboundConnectionError::TlsHandshake(
                                "Unexpected response to STARTTLS".to_string(),
                            ));
                        }
                    }
                }
            }
        }

        // Take the TCP stream and upgrade to TLS
        let tcp_stream = match std::mem::replace(&mut self.stream, None) {
            Some(OutboundStream::Tcp(s)) => s,
            _ => {
                return Err(OutboundConnectionError::TlsHandshake(
                    "No TCP stream available for TLS upgrade".to_string(),
                ))
            }
        };

        // Perform TLS handshake
        let server_name = rustls::pki_types::ServerName::try_from(server_name.to_string())
            .map_err(|e| {
                OutboundConnectionError::TlsHandshake(format!("Invalid server name: {}", e))
            })?;

        let tls_stream = tls_connector
            .connect(server_name, tcp_stream)
            .await
            .map_err(|e| {
                OutboundConnectionError::TlsHandshake(format!("TLS handshake failed: {}", e))
            })?;

        self.stream = Some(OutboundStream::Tls(Box::new(tls_stream)));
        self.parser.reset();

        info!("S2S TLS upgrade complete");

        Ok(())
    }

    /// Wait for and validate dialback feature.
    async fn expect_dialback_feature(&mut self) -> Result<(), OutboundConnectionError> {
        let mut buf = [0u8; 4096];

        loop {
            let n = self.read(&mut buf).await?;
            if n == 0 {
                return Err(OutboundConnectionError::ConnectionClosed);
            }

            self.parser.feed(&buf[..n]);

            if self.parser.has_complete_stanza() {
                if let Some(stanza) = self
                    .parser
                    .next_stanza()
                    .map_err(|e| OutboundConnectionError::StreamNegotiation(e.to_string()))?
                {
                    match stanza {
                        ParsedStanza::Features { dialback, .. } => {
                            if dialback {
                                debug!("Remote server advertises dialback");
                                return Ok(());
                            } else {
                                // Some servers might not advertise dialback but still support it
                                // We'll try anyway
                                warn!(
                                    "Remote server does not advertise dialback, attempting anyway"
                                );
                                return Ok(());
                            }
                        }
                        _ => {
                            continue;
                        }
                    }
                }
            }
        }
    }

    /// Perform Server Dialback authentication (XEP-0220).
    async fn perform_dialback(&mut self) -> Result<(), OutboundConnectionError> {
        self.dialback_state = DialbackState::Pending;

        // Generate dialback key
        let stream_id = self.stream_id.as_ref().ok_or_else(|| {
            OutboundConnectionError::DialbackFailed("No stream ID available".to_string())
        })?;

        let key = self
            .dialback_key
            .generate(stream_id, &self.remote_domain, &self.local_domain);

        // Send db:result with key
        let db_result = build_db_result(&self.local_domain, &self.remote_domain, &key);
        self.write_all(db_result.as_bytes()).await?;
        self.flush().await?;

        debug!(
            from = %self.local_domain,
            to = %self.remote_domain,
            "Sent dialback result"
        );

        // Wait for db:result response
        let mut buf = [0u8; 4096];

        loop {
            let n = self.read(&mut buf).await?;
            if n == 0 {
                return Err(OutboundConnectionError::ConnectionClosed);
            }

            self.parser.feed(&buf[..n]);

            while self.parser.has_complete_stanza() {
                if let Some(stanza) = self
                    .parser
                    .next_stanza()
                    .map_err(|e| OutboundConnectionError::DialbackFailed(e.to_string()))?
                {
                    match stanza {
                        ParsedStanza::DialbackResult {
                            from,
                            to,
                            result_type,
                            ..
                        } => {
                            // Validate the response is for us
                            if from != self.remote_domain || to != self.local_domain {
                                warn!(
                                    expected_from = %self.remote_domain,
                                    got_from = %from,
                                    expected_to = %self.local_domain,
                                    got_to = %to,
                                    "Dialback result domain mismatch"
                                );
                                continue;
                            }

                            match result_type.as_deref() {
                                Some("valid") => {
                                    self.dialback_state = DialbackState::Verified;
                                    info!("Dialback authentication successful");
                                    return Ok(());
                                }
                                Some("invalid") => {
                                    self.dialback_state = DialbackState::Failed;
                                    return Err(OutboundConnectionError::DialbackFailed(
                                        "Dialback verification failed".to_string(),
                                    ));
                                }
                                other => {
                                    return Err(OutboundConnectionError::DialbackFailed(format!(
                                        "Unexpected dialback result type: {:?}",
                                        other
                                    )));
                                }
                            }
                        }
                        ParsedStanza::StreamError { condition, text } => {
                            return Err(OutboundConnectionError::Protocol(format!(
                                "Stream error: {} - {:?}",
                                condition, text
                            )));
                        }
                        _ => {
                            debug!(?stanza, "Ignoring stanza during dialback");
                        }
                    }
                }
            }
        }
    }

    /// Read bytes from the underlying stream.
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, OutboundConnectionError> {
        match &mut self.stream {
            Some(OutboundStream::Tcp(s)) => Ok(s.read(buf).await?),
            Some(OutboundStream::Tls(s)) => Ok(s.read(buf).await?),
            None => Err(OutboundConnectionError::ConnectionClosed),
        }
    }

    /// Write bytes to the underlying stream.
    async fn write_all(&mut self, buf: &[u8]) -> Result<(), OutboundConnectionError> {
        match &mut self.stream {
            Some(OutboundStream::Tcp(s)) => Ok(s.write_all(buf).await?),
            Some(OutboundStream::Tls(s)) => Ok(s.write_all(buf).await?),
            None => Err(OutboundConnectionError::ConnectionClosed),
        }
    }

    /// Flush the write buffer.
    async fn flush(&mut self) -> Result<(), OutboundConnectionError> {
        match &mut self.stream {
            Some(OutboundStream::Tcp(s)) => Ok(s.flush().await?),
            Some(OutboundStream::Tls(s)) => Ok(s.flush().await?),
            None => Err(OutboundConnectionError::ConnectionClosed),
        }
    }

    /// Check if the connection is still alive.
    pub fn is_connected(&self) -> bool {
        self.connected.load(Ordering::Relaxed) && self.state == OutboundState::Established
    }

    /// Get the remote domain.
    pub fn remote_domain(&self) -> &str {
        &self.remote_domain
    }

    /// Get the local domain.
    pub fn local_domain(&self) -> &str {
        &self.local_domain
    }

    /// Get the current connection state.
    pub fn state(&self) -> OutboundState {
        self.state
    }

    /// Get the stream ID.
    pub fn stream_id(&self) -> Option<&str> {
        self.stream_id.as_deref()
    }

    /// Get when the connection was established.
    pub fn connected_at(&self) -> Option<Instant> {
        self.connected_at
    }

    /// Send raw bytes through the connection.
    ///
    /// This should only be used for sending properly formatted XMPP stanzas.
    pub async fn send_raw(&mut self, data: &[u8]) -> Result<(), OutboundConnectionError> {
        if !self.is_connected() {
            return Err(OutboundConnectionError::ConnectionClosed);
        }

        self.write_all(data).await?;
        self.flush().await?;
        Ok(())
    }

    /// Close the connection gracefully.
    pub async fn close(&mut self) -> Result<(), OutboundConnectionError> {
        if self.state == OutboundState::Closed {
            return Ok(());
        }

        debug!(
            remote_domain = %self.remote_domain,
            "Closing S2S outbound connection"
        );

        // Send stream end
        if let Err(e) = self.write_all(b"</stream:stream>").await {
            warn!(error = %e, "Error sending stream end");
        }
        let _ = self.flush().await;

        self.state = OutboundState::Closed;
        self.connected.store(false, Ordering::Relaxed);

        info!(
            remote_domain = %self.remote_domain,
            "S2S outbound connection closed"
        );

        Ok(())
    }
}

impl Drop for S2sOutboundConnection {
    fn drop(&mut self) {
        self.connected.store(false, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_outbound_state() {
        assert_eq!(OutboundState::Disconnected, OutboundState::Disconnected);
        assert_ne!(OutboundState::Established, OutboundState::Closed);
    }

    #[test]
    fn test_outbound_connection_error_display() {
        let err = OutboundConnectionError::ConnectionClosed;
        assert_eq!(err.to_string(), "Connection closed unexpectedly");

        let err = OutboundConnectionError::DialbackFailed("test".to_string());
        assert!(err.to_string().contains("test"));
    }
}
