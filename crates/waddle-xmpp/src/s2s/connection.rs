//! S2S (Server-to-Server) connection handling.
//!
//! Manages individual connections from remote XMPP servers.
//! Implements stream negotiation with TLS 1.3 and feature advertisement.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::server::TlsStream;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, instrument, warn};

use crate::parser::{ns, StreamHeader, XmlParser};
use crate::s2s::{S2sMetrics, S2sState};
use crate::XmppError;

/// Actor managing a single S2S connection from a remote server.
pub struct S2sConnectionActor {
    /// Peer address
    peer_addr: SocketAddr,
    /// The underlying stream (either TCP or TLS)
    inner: S2sStreamInner,
    /// Incremental XML parser
    parser: XmlParser,
    /// Current connection state
    state: S2sState,
    /// Local server domain
    local_domain: String,
    /// Remote server domain (from stream header)
    remote_domain: Option<String>,
    /// Current stream ID
    stream_id: String,
    /// Metrics for tracking S2S connections
    metrics: Arc<S2sMetrics>,
}

/// Inner stream type for S2S connections.
#[derive(Default)]
enum S2sStreamInner {
    #[default]
    None,
    Tcp(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl S2sConnectionActor {
    /// Handle a new incoming S2S connection.
    #[instrument(
        name = "xmpp.s2s.connection.handle",
        skip(tcp_stream, tls_acceptor, metrics),
        fields(peer = %peer_addr)
    )]
    pub async fn handle_connection(
        tcp_stream: TcpStream,
        peer_addr: SocketAddr,
        tls_acceptor: TlsAcceptor,
        local_domain: String,
        metrics: Arc<S2sMetrics>,
    ) -> Result<(), XmppError> {
        info!("New S2S connection from {}", peer_addr);

        let mut actor = Self {
            peer_addr,
            inner: S2sStreamInner::Tcp(tcp_stream),
            parser: XmlParser::new(),
            state: S2sState::Initial,
            local_domain,
            remote_domain: None,
            stream_id: uuid::Uuid::new_v4().to_string(),
            metrics,
        };

        actor.run(tls_acceptor).await
    }

    /// Main connection loop.
    async fn run(&mut self, tls_acceptor: TlsAcceptor) -> Result<(), XmppError> {
        // Increment active connection count
        self.metrics.record_connection_established();

        let result = self.run_inner(tls_acceptor).await;

        // Decrement active connection count
        self.metrics.record_connection_closed();

        // Update state
        self.state = S2sState::Closed;
        info!(
            remote_domain = ?self.remote_domain,
            "S2S connection closed"
        );

        result
    }

    /// Inner run loop with negotiation logic.
    async fn run_inner(&mut self, tls_acceptor: TlsAcceptor) -> Result<(), XmppError> {
        // Wait for initial stream header
        let header = self.read_stream_header().await?;

        // Extract the remote domain from 'from' attribute
        if let Some(ref from) = header.from {
            self.remote_domain = Some(from.clone());
            debug!(remote_domain = %from, "Remote domain identified");

            // Record in span
            tracing::Span::current().record("remote_domain", from.as_str());
        }

        // Validate the 'to' attribute matches our domain
        if let Some(ref to) = header.to {
            if to != &self.local_domain {
                debug!(expected = %self.local_domain, got = %to, "S2S domain mismatch in stream header");
                // Per RFC 6120, we should still proceed but may reject later
            }
        }

        // Send stream features (STARTTLS required for S2S)
        self.send_features_starttls().await?;

        // Wait for STARTTLS
        self.state = S2sState::Initial;
        self.handle_starttls(tls_acceptor).await?;

        // TLS established, wait for new stream header
        let _header = self.read_stream_header().await?;

        // Send post-TLS features (dialback, etc.)
        self.send_features_dialback().await?;

        // Wait for dialback or other authentication mechanism
        self.state = S2sState::Dialback;

        // For now, we'll wait for stanzas and handle them in a simple loop
        // Full dialback implementation will be added in a later phase
        self.process_s2s_stanzas().await?;

        Ok(())
    }

    /// Read bytes from the underlying stream.
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, XmppError> {
        match &mut self.inner {
            S2sStreamInner::None => Err(XmppError::internal("Stream not initialized")),
            S2sStreamInner::Tcp(s) => Ok(s.read(buf).await?),
            S2sStreamInner::Tls(s) => Ok(s.read(buf).await?),
        }
    }

    /// Write bytes to the underlying stream.
    async fn write_all(&mut self, buf: &[u8]) -> Result<(), XmppError> {
        match &mut self.inner {
            S2sStreamInner::None => Err(XmppError::internal("Stream not initialized")),
            S2sStreamInner::Tcp(s) => Ok(s.write_all(buf).await?),
            S2sStreamInner::Tls(s) => Ok(s.write_all(buf).await?),
        }
    }

    /// Flush the write buffer.
    async fn flush(&mut self) -> Result<(), XmppError> {
        match &mut self.inner {
            S2sStreamInner::None => Err(XmppError::internal("Stream not initialized")),
            S2sStreamInner::Tcp(s) => Ok(s.flush().await?),
            S2sStreamInner::Tls(s) => Ok(s.flush().await?),
        }
    }

    /// Read data until we have a complete stream header.
    #[instrument(skip(self), name = "xmpp.s2s.stream.read_header")]
    async fn read_stream_header(&mut self) -> Result<StreamHeader, XmppError> {
        // Reset parser for new stream
        self.parser.reset();
        self.stream_id = uuid::Uuid::new_v4().to_string();

        let mut buf = [0u8; 4096];

        // Read until we have a complete stream header
        loop {
            let n = self.read(&mut buf).await?;

            if n == 0 {
                return Err(XmppError::stream("Connection closed during header"));
            }

            self.parser.feed(&buf[..n]);

            if self.parser.has_stream_header() {
                break;
            }
        }

        let header = self.parser.take_stream_header()?;
        header.validate()?;

        debug!(
            to = ?header.to,
            from = ?header.from,
            version = ?header.version,
            "Received S2S stream header"
        );

        // Send our stream header response
        self.send_stream_header().await?;

        Ok(header)
    }

    /// Send the server's stream header for S2S.
    async fn send_stream_header(&mut self) -> Result<(), XmppError> {
        // S2S streams use xmlns='jabber:server' instead of 'jabber:client'
        let response = format!(
            "<?xml version='1.0'?>\
            <stream:stream xmlns='jabber:server' \
            xmlns:stream='http://etherx.jabber.org/streams' \
            xmlns:db='jabber:server:dialback' \
            id='{}' from='{}' version='1.0'>",
            self.stream_id, self.local_domain
        );

        self.write_all(response.as_bytes()).await?;
        self.flush().await?;

        debug!(stream_id = %self.stream_id, "Sent S2S stream header");
        Ok(())
    }

    /// Send stream features advertising STARTTLS.
    #[instrument(skip(self), name = "xmpp.s2s.stream.send_features_starttls")]
    async fn send_features_starttls(&mut self) -> Result<(), XmppError> {
        // For S2S, STARTTLS is required per RFC 6120 Section 13.10
        let features = format!(
            "<stream:features>\
                <starttls xmlns='{}'>\
                    <required/>\
                </starttls>\
            </stream:features>",
            ns::TLS
        );

        self.write_all(features.as_bytes()).await?;
        self.flush().await?;

        debug!("Sent S2S STARTTLS features");
        Ok(())
    }

    /// Handle STARTTLS upgrade for S2S connection.
    #[instrument(skip(self, tls_acceptor), name = "xmpp.s2s.stream.starttls")]
    async fn handle_starttls(&mut self, tls_acceptor: TlsAcceptor) -> Result<(), XmppError> {
        // Read until we get a starttls request
        let mut buf = [0u8; 1024];

        loop {
            let n = match &mut self.inner {
                S2sStreamInner::None => return Err(XmppError::internal("Stream not initialized")),
                S2sStreamInner::Tcp(s) => s.read(&mut buf).await?,
                S2sStreamInner::Tls(_) => return Err(XmppError::stream("Already using TLS")),
            };

            if n == 0 {
                return Err(XmppError::stream("Connection closed during STARTTLS"));
            }

            self.parser.feed(&buf[..n]);

            if self.parser.has_complete_stanza() {
                if let Some(crate::parser::ParsedStanza::StartTls) = self.parser.next_stanza()? {
                    break;
                }
            }
        }

        debug!("Received S2S STARTTLS request");

        // Send proceed
        let proceed = format!("<proceed xmlns='{}'/>", ns::TLS);
        match &mut self.inner {
            S2sStreamInner::None => return Err(XmppError::internal("Stream not initialized")),
            S2sStreamInner::Tcp(s) => {
                s.write_all(proceed.as_bytes()).await?;
                s.flush().await?;
            }
            S2sStreamInner::Tls(_) => return Err(XmppError::stream("Already using TLS")),
        }

        // Upgrade to TLS - take ownership of the TCP stream
        let tcp_stream = match std::mem::take(&mut self.inner) {
            S2sStreamInner::Tcp(s) => s,
            S2sStreamInner::Tls(_) => return Err(XmppError::stream("Already using TLS")),
            S2sStreamInner::None => return Err(XmppError::internal("Stream already taken")),
        };

        let tls_stream = tls_acceptor
            .accept(tcp_stream)
            .await
            .map_err(|e| XmppError::internal(format!("S2S TLS accept error: {}", e)))?;

        self.inner = S2sStreamInner::Tls(Box::new(tls_stream));
        self.parser.reset();

        // Record TLS upgrade success
        self.metrics.record_tls_established();

        debug!("S2S TLS upgrade complete");

        Ok(())
    }

    /// Send stream features advertising dialback and other S2S features.
    #[instrument(skip(self), name = "xmpp.s2s.stream.send_features_dialback")]
    async fn send_features_dialback(&mut self) -> Result<(), XmppError> {
        // Advertise Server Dialback (XEP-0220) and other S2S features
        let features = r#"<stream:features>
            <dialback xmlns='urn:xmpp:features:dialback'>
                <errors/>
            </dialback>
        </stream:features>"#;

        self.write_all(features.as_bytes()).await?;
        self.flush().await?;

        debug!("Sent S2S post-TLS features (dialback)");
        Ok(())
    }

    /// Process incoming S2S stanzas.
    ///
    /// This is a basic implementation that handles the initial negotiation.
    /// Full stanza routing will be implemented in a later phase.
    #[instrument(skip(self), name = "xmpp.s2s.process_stanzas")]
    async fn process_s2s_stanzas(&mut self) -> Result<(), XmppError> {
        let mut buf = [0u8; 8192];

        loop {
            let n = self.read(&mut buf).await?;

            if n == 0 {
                debug!("S2S connection closed by remote");
                return Ok(());
            }

            self.parser.feed(&buf[..n]);

            while self.parser.has_complete_stanza() {
                match self.parser.next_stanza()? {
                    Some(stanza) => {
                        debug!(?stanza, "Received S2S stanza");
                        self.handle_s2s_stanza(&stanza).await?;
                    }
                    None => break,
                }
            }
        }
    }

    /// Handle an individual S2S stanza.
    async fn handle_s2s_stanza(
        &mut self,
        stanza: &crate::parser::ParsedStanza,
    ) -> Result<(), XmppError> {
        use crate::parser::ParsedStanza;

        match stanza {
            ParsedStanza::StreamEnd => {
                debug!("Received stream end from remote server");
                self.send_stream_end().await?;
                return Ok(());
            }
            ParsedStanza::Iq(_iq) => {
                // Handle IQ stanzas (disco, etc.)
                debug!("Received S2S IQ - not yet implemented");
                // TODO: Implement S2S IQ handling
            }
            ParsedStanza::Message(_msg) => {
                // Handle message routing from remote server
                debug!("Received S2S message - not yet implemented");
                // TODO: Implement message routing to local users
            }
            ParsedStanza::Presence(_presence) => {
                // Handle presence from remote server
                debug!("Received S2S presence - not yet implemented");
                // TODO: Implement presence routing
            }
            ParsedStanza::Unknown(elem) => {
                // Check for dialback elements
                let name = elem.name();
                let xmlns = elem.ns();

                if xmlns == "jabber:server:dialback" {
                    match name {
                        "result" => {
                            debug!("Received dialback result - not yet implemented");
                            // TODO: Implement dialback result handling
                        }
                        "verify" => {
                            debug!("Received dialback verify - not yet implemented");
                            // TODO: Implement dialback verification
                        }
                        _ => {
                            warn!(name = %name, "Unknown dialback element");
                        }
                    }
                } else {
                    debug!(
                        name = %name,
                        xmlns = %xmlns,
                        "Unknown S2S element"
                    );
                }
            }
            _ => {
                debug!(?stanza, "Unhandled S2S stanza type");
            }
        }

        Ok(())
    }

    /// Send stream end element.
    async fn send_stream_end(&mut self) -> Result<(), XmppError> {
        self.write_all(b"</stream:stream>").await?;
        self.flush().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_s2s_state_transitions() {
        // Test state enum values
        assert_eq!(S2sState::Initial, S2sState::Initial);
        assert_ne!(S2sState::Initial, S2sState::Established);
    }
}
