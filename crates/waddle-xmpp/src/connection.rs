//! Connection actor for handling individual XMPP client connections.

use std::net::SocketAddr;
use std::sync::Arc;

use jid::FullJid;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, instrument, warn};
use xmpp_parsers::message::MessageType;

use crate::muc::{MucMessage, MucRoomRegistry};
use crate::registry::{ConnectionRegistry, OutboundStanza, SendResult};
use crate::stream::XmppStream;
use crate::types::ConnectionState;
use crate::{AppState, Session, XmppError};

/// Size of the outbound message channel buffer.
const OUTBOUND_CHANNEL_SIZE: usize = 256;

/// Receiver for outbound stanzas to be sent to this connection.
type OutboundReceiver = mpsc::Receiver<OutboundStanza>;

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
    /// MUC room registry for groupchat message routing
    room_registry: Arc<MucRoomRegistry>,
    /// Connection registry for message routing between connections
    connection_registry: Arc<ConnectionRegistry>,
    /// Receiver for outbound stanzas (messages routed to this connection)
    outbound_rx: Option<OutboundReceiver>,
}

impl<S: AppState> ConnectionActor<S> {
    /// Handle a new incoming connection.
    #[instrument(
        name = "xmpp.connection.handle",
        skip(tcp_stream, tls_acceptor, app_state, room_registry, connection_registry),
        fields(peer = %peer_addr)
    )]
    pub async fn handle_connection(
        tcp_stream: TcpStream,
        peer_addr: SocketAddr,
        tls_acceptor: TlsAcceptor,
        domain: String,
        app_state: Arc<S>,
        room_registry: Arc<MucRoomRegistry>,
        connection_registry: Arc<ConnectionRegistry>,
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
            room_registry,
            connection_registry,
            outbound_rx: None,
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

        // Register this connection with the connection registry for message routing
        let (outbound_tx, outbound_rx) = mpsc::channel(OUTBOUND_CHANNEL_SIZE);
        self.connection_registry.register(full_jid.clone(), outbound_tx);
        self.outbound_rx = Some(outbound_rx);

        info!(jid = %full_jid, "Session established and registered");

        // Main stanza processing loop
        let result = self.process_stanzas().await;

        // Unregister this connection on disconnect
        if let Some(ref jid) = self.jid {
            self.connection_registry.unregister(jid);
            debug!(jid = %jid, "Unregistered connection");
        }

        self.state = ConnectionState::Closed;
        info!("Connection closed");

        result
    }

    /// Process stanzas until the connection is closed.
    ///
    /// This function handles both:
    /// - Inbound stanzas from the client (messages, presence, IQs)
    /// - Outbound stanzas routed from other connections via the registry
    async fn process_stanzas(&mut self) -> Result<(), XmppError> {
        // Take ownership of the outbound receiver
        let mut outbound_rx = self.outbound_rx.take();

        loop {
            tokio::select! {
                // Handle inbound stanzas from the client
                inbound_result = self.stream.read_stanza() => {
                    match inbound_result {
                        Ok(Some(stanza)) => {
                            if let Err(e) = self.handle_stanza(stanza).await {
                                warn!(error = %e, "Error handling stanza");
                            }
                        }
                        Ok(None) => {
                            // Stream closed gracefully
                            debug!("Client closed stream");
                            break;
                        }
                        Err(e) => {
                            warn!(error = %e, "Error reading stanza");
                            break;
                        }
                    }
                }

                // Handle outbound stanzas routed from other connections
                outbound = async {
                    match outbound_rx.as_mut() {
                        Some(rx) => rx.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match outbound {
                        Some(outbound_stanza) => {
                            debug!("Received outbound stanza from registry");
                            if let Err(e) = self.stream.write_stanza(&outbound_stanza.stanza).await {
                                warn!(error = %e, "Error writing outbound stanza");
                                // Don't break - the client might still be readable
                            }
                        }
                        None => {
                            // Outbound channel closed - this shouldn't happen during normal operation
                            debug!("Outbound channel closed");
                        }
                    }
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

    /// Handle an incoming message stanza.
    ///
    /// Routes messages based on type:
    /// - Groupchat: Route to MUC room for broadcasting to all occupants
    /// - Chat: Direct message to another user (TODO)
    /// - Other: Currently logged and ignored
    #[instrument(skip(self, msg), fields(msg_type = ?msg.type_, to = ?msg.to))]
    async fn handle_message(
        &mut self,
        msg: xmpp_parsers::message::Message,
    ) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => {
                warn!("Message received before JID bound");
                return Err(XmppError::not_authorized(Some(
                    "Session not established".to_string(),
                )));
            }
        };

        // Route based on message type
        match msg.type_ {
            MessageType::Groupchat => {
                self.handle_groupchat_message(msg, sender_jid).await
            }
            MessageType::Chat => {
                // TODO: Implement direct chat message routing
                debug!("Received chat message (not yet implemented)");
                Ok(())
            }
            MessageType::Normal | MessageType::Headline | MessageType::Error => {
                debug!(msg_type = ?msg.type_, "Received message of unsupported type");
                Ok(())
            }
        }
    }

    /// Handle a groupchat (MUC) message.
    ///
    /// Routes the message to the appropriate MUC room, which broadcasts
    /// it to all occupants (including sending an echo back to the sender
    /// per XEP-0045).
    #[instrument(skip(self, msg), fields(room = ?msg.to))]
    async fn handle_groupchat_message(
        &mut self,
        msg: xmpp_parsers::message::Message,
        sender_jid: FullJid,
    ) -> Result<(), XmppError> {
        // Parse the MUC message
        let muc_msg = MucMessage::from_message(msg, sender_jid.clone())?;
        let room_jid = muc_msg.room_jid.clone();

        debug!(
            room = %room_jid,
            sender = %sender_jid,
            has_body = muc_msg.has_body(),
            "Routing groupchat message"
        );

        // Check if this is a MUC room we manage
        if !self.room_registry.is_muc_jid(&room_jid) {
            debug!(
                room = %room_jid,
                muc_domain = %self.room_registry.muc_domain(),
                "Message to non-MUC JID"
            );
            return Err(XmppError::item_not_found(Some(format!(
                "Room {} not found",
                room_jid
            ))));
        }

        // Get the room data
        let room_data = self.room_registry.get_room_data(&room_jid).ok_or_else(|| {
            debug!(room = %room_jid, "Room not found in registry");
            XmppError::item_not_found(Some(format!("Room {} not found", room_jid)))
        })?;

        // Read the room and broadcast the message
        let room = room_data.read().await;

        // Find the sender's nick in the room
        let sender_nick = room.find_nick_by_real_jid(&sender_jid).ok_or_else(|| {
            debug!(
                sender = %sender_jid,
                room = %room_jid,
                "Sender is not an occupant of the room"
            );
            XmppError::forbidden(Some(format!(
                "You are not an occupant of {}",
                room_jid
            )))
        })?;

        // Broadcast the message to all occupants
        let outbound_messages = room.broadcast_message(sender_nick, &muc_msg.message)?;

        drop(room); // Release the read lock before sending

        // Send the messages to all occupants via the connection registry
        for outbound in &outbound_messages {
            if outbound.to == sender_jid {
                // This is the echo back to the sender - write directly to our stream
                debug!(to = %outbound.to, "Sending message echo to sender");
                self.stream
                    .write_stanza(&Stanza::Message(outbound.message.clone()))
                    .await?;
            } else {
                // Route to other occupants via the connection registry
                let stanza = Stanza::Message(outbound.message.clone());
                let result = self.connection_registry.send_to(&outbound.to, stanza).await;

                match result {
                    SendResult::Sent => {
                        debug!(to = %outbound.to, "Message routed to occupant");
                    }
                    SendResult::NotConnected => {
                        debug!(
                            to = %outbound.to,
                            "Occupant not connected, message not delivered"
                        );
                        // In a full implementation, we might queue for offline delivery
                    }
                    SendResult::ChannelFull => {
                        warn!(
                            to = %outbound.to,
                            "Occupant's channel full, message dropped"
                        );
                    }
                    SendResult::ChannelClosed => {
                        debug!(
                            to = %outbound.to,
                            "Occupant's channel closed, message not delivered"
                        );
                    }
                }
            }
        }

        debug!(
            room = %room_jid,
            recipient_count = outbound_messages.len(),
            "Groupchat message processed"
        );

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
