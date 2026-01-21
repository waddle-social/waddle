//! Connection actor for handling individual XMPP client connections.

use std::net::SocketAddr;
use std::sync::Arc;

use jid::FullJid;
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, instrument, warn};

use crate::stream::XmppStream;
use crate::types::ConnectionState;
use crate::{AppState, Session, XmppError};

/// Actor managing a single XMPP client connection.
pub struct ConnectionActor<S: AppState> {
    /// Peer address
    _peer_addr: SocketAddr,
    /// XMPP stream handler
    stream: XmppStream,
    /// Current connection state
    state: ConnectionState,
    /// Authenticated session (after SASL)
    session: Option<Session>,
    /// Bound JID with resource
    jid: Option<FullJid>,
    /// Server domain
    domain: String,
    /// Shared application state
    app_state: Arc<S>,
}

impl<S: AppState> ConnectionActor<S> {
    /// Handle a new incoming connection.
    #[instrument(
        name = "xmpp.connection.handle",
        skip(tcp_stream, tls_acceptor, app_state),
        fields(peer = %peer_addr)
    )]
    pub async fn handle_connection(
        tcp_stream: TcpStream,
        peer_addr: SocketAddr,
        tls_acceptor: TlsAcceptor,
        domain: String,
        app_state: Arc<S>,
    ) -> Result<(), XmppError> {
        info!("New connection from {}", peer_addr);

        let mut actor = Self {
            _peer_addr: peer_addr,
            stream: XmppStream::new(tcp_stream, domain.clone()),
            state: ConnectionState::Initial,
            session: None,
            jid: None,
            domain,
            app_state,
        };

        actor.run(tls_acceptor).await
    }

    /// Main connection loop.
    async fn run(&mut self, tls_acceptor: TlsAcceptor) -> Result<(), XmppError> {
        // Wait for initial stream header
        self.state = ConnectionState::Negotiating;
        let header = self.stream.read_stream_header().await?;

        // Validate the 'to' attribute matches our domain
        if let Some(ref to) = header.to {
            if to != &self.domain {
                debug!(expected = %self.domain, got = %to, "Domain mismatch in stream header");
                // Continue anyway, but log it
            }
        }

        // Send stream features (STARTTLS required)
        self.stream.send_features_starttls().await?;

        // Wait for STARTTLS
        self.state = ConnectionState::StartTls;
        self.stream.handle_starttls(tls_acceptor).await?;

        // TLS established, send new features (SASL)
        self.state = ConnectionState::TlsEstablished;
        let _header = self.stream.read_stream_header().await?;
        self.stream.send_features_sasl().await?;

        // Handle SASL authentication
        self.state = ConnectionState::Authenticating;
        let (jid, token) = self.stream.handle_sasl_auth().await?;

        // Validate session with app state
        let session = self
            .app_state
            .validate_session(&jid.clone().into(), &token)
            .await?;
        self.session = Some(session);
        self.state = ConnectionState::Authenticated;

        debug!(jid = %jid, "Authentication successful");

        // Record the JID in the parent span (xmpp.connection.lifecycle)
        tracing::Span::current().record("jid", jid.to_string());

        // Stream restart after SASL
        let _header = self.stream.read_stream_header().await?;
        self.stream.send_features_bind().await?;

        // Resource binding
        let full_jid = self.stream.handle_bind(&jid).await?;
        self.jid = Some(full_jid.clone());
        self.state = ConnectionState::Established;

        info!(jid = %full_jid, "Session established");

        // Main stanza processing loop
        self.process_stanzas().await?;

        self.state = ConnectionState::Closed;
        info!("Connection closed");

        Ok(())
    }

    /// Process stanzas until the connection is closed.
    async fn process_stanzas(&mut self) -> Result<(), XmppError> {
        loop {
            match self.stream.read_stanza().await {
                Ok(Some(stanza)) => {
                    if let Err(e) = self.handle_stanza(stanza).await {
                        warn!(error = %e, "Error handling stanza");
                    }
                }
                Ok(None) => {
                    // Stream closed gracefully
                    break;
                }
                Err(e) => {
                    warn!(error = %e, "Error reading stanza");
                    break;
                }
            }
        }

        Ok(())
    }

    /// Handle a single stanza.
    #[instrument(
        name = "xmpp.stanza.process",
        skip(self, stanza),
        fields(
            stanza_type = %stanza.name(),
            from = %self.jid.as_ref().map(|j| j.to_string()).unwrap_or_default(),
            to = tracing::field::Empty,  // Set per-stanza when available
        )
    )]
    async fn handle_stanza(&mut self, stanza: Stanza) -> Result<(), XmppError> {
        match stanza {
            Stanza::Message(msg) => self.handle_message(msg).await,
            Stanza::Presence(pres) => self.handle_presence(pres).await,
            Stanza::Iq(iq) => self.handle_iq(iq).await,
        }
    }

    async fn handle_message(
        &mut self,
        _msg: xmpp_parsers::message::Message,
    ) -> Result<(), XmppError> {
        // TODO: Implement message routing
        debug!("Received message stanza");
        Ok(())
    }

    async fn handle_presence(
        &mut self,
        _pres: xmpp_parsers::presence::Presence,
    ) -> Result<(), XmppError> {
        // TODO: Implement presence handling
        debug!("Received presence stanza");
        Ok(())
    }

    async fn handle_iq(&mut self, _iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        // TODO: Implement IQ handling (disco, ping, etc.)
        debug!("Received IQ stanza");
        Ok(())
    }
}

/// Parsed stanza types.
#[derive(Debug, Clone)]
pub enum Stanza {
    Message(xmpp_parsers::message::Message),
    Presence(xmpp_parsers::presence::Presence),
    Iq(xmpp_parsers::iq::Iq),
}

impl Stanza {
    /// Get the stanza type name for tracing.
    pub fn name(&self) -> &'static str {
        match self {
            Stanza::Message(_) => "message",
            Stanza::Presence(_) => "presence",
            Stanza::Iq(_) => "iq",
        }
    }
}
