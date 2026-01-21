//! Connection actor for handling individual XMPP client connections.

use std::net::SocketAddr;
use std::sync::Arc;

use jid::FullJid;
use tokio::net::TcpStream;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, instrument, warn};
use xmpp_parsers::message::MessageType;

use crate::muc::{MucMessage, MucRoomRegistry};
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
    /// MUC room registry for groupchat message routing
    room_registry: Arc<MucRoomRegistry>,
}

impl<S: AppState> ConnectionActor<S> {
    /// Handle a new incoming connection.
    #[instrument(
        name = "xmpp.connection.handle",
        skip(tcp_stream, tls_acceptor, app_state, room_registry),
        fields(peer = %peer_addr)
    )]
    pub async fn handle_connection(
        tcp_stream: TcpStream,
        peer_addr: SocketAddr,
        tls_acceptor: TlsAcceptor,
        domain: String,
        app_state: Arc<S>,
        room_registry: Arc<MucRoomRegistry>,
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

        // Send the messages to all occupants
        // Note: In a full implementation, we'd route these to the appropriate
        // ConnectionActors. For now, we write the messages that go to _this_
        // connection (the sender's echo).
        for outbound in &outbound_messages {
            if outbound.to == sender_jid {
                // This is the echo back to the sender
                debug!(to = %outbound.to, "Sending message echo to sender");
                self.stream
                    .write_stanza(&Stanza::Message(outbound.message.clone()))
                    .await?;
            } else {
                // Messages to other occupants would be routed via a connection
                // registry or message broker. For now, log them.
                debug!(
                    to = %outbound.to,
                    "Would send message to occupant (routing not yet implemented)"
                );
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
