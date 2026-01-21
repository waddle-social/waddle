//! Connection actor for handling individual XMPP client connections.

use std::net::SocketAddr;
use std::sync::Arc;

use chrono::Utc;
use jid::FullJid;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, instrument, warn};
use xmpp_parsers::message::MessageType;

use crate::mam::{
    add_stanza_id, build_fin_iq, build_result_messages, is_mam_query, parse_mam_query,
    ArchivedMessage, MamStorage,
};
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
pub struct ConnectionActor<S: AppState, M: MamStorage> {
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
    /// MAM storage for message archival
    mam_storage: Arc<M>,
    /// Receiver for outbound stanzas (messages routed to this connection)
    outbound_rx: Option<OutboundReceiver>,
}

impl<S: AppState, M: MamStorage> ConnectionActor<S, M> {
    /// Handle a new incoming connection.
    #[instrument(
        name = "xmpp.connection.handle",
        skip(tcp_stream, tls_acceptor, app_state, room_registry, connection_registry, mam_storage),
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
        mam_storage: Arc<M>,
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
            mam_storage,
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
    /// per XEP-0045). Also archives the message to MAM storage.
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
        let mut outbound_messages = room.broadcast_message(sender_nick.clone(), &muc_msg.message)?;

        drop(room); // Release the read lock before archival and sending

        // Archive the message to MAM storage (only if it has a body)
        let archive_id = if muc_msg.has_body() {
            // Build the sender's MUC JID (room JID + nick resource)
            let muc_sender_jid = format!("{}/{}", room_jid, sender_nick);

            // Extract the body text for archival
            let body = muc_msg
                .message
                .bodies
                .get("")
                .or_else(|| muc_msg.message.bodies.values().next())
                .map(|b| b.0.clone())
                .unwrap_or_default();

            let archived_msg = ArchivedMessage {
                id: String::new(), // Let storage generate ID
                timestamp: Utc::now(),
                from: muc_sender_jid,
                to: room_jid.to_string(),
                body,
                stanza_id: muc_msg.message.id.clone(),
            };

            match self.mam_storage.store_message(&archived_msg).await {
                Ok(id) => {
                    debug!(
                        archive_id = %id,
                        room = %room_jid,
                        "Message archived to MAM storage"
                    );
                    Some(id)
                }
                Err(e) => {
                    warn!(
                        error = %e,
                        room = %room_jid,
                        "Failed to archive message to MAM storage"
                    );
                    // Don't fail the message delivery if archival fails
                    None
                }
            }
        } else {
            None
        };

        // Add stanza-id to outbound messages if we archived successfully
        if let Some(ref archive_id) = archive_id {
            for outbound in &mut outbound_messages {
                add_stanza_id(&mut outbound.message, archive_id, &room_jid.to_string());
            }
        }

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
            archived = archive_id.is_some(),
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

    /// Handle an IQ stanza.
    ///
    /// Currently supports:
    /// - MAM queries (XEP-0313)
    #[instrument(skip(self, iq), fields(iq_type = ?iq.payload, iq_id = %iq.id))]
    async fn handle_iq(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        // Check if this is a MAM query
        if is_mam_query(&iq) {
            return self.handle_mam_query(iq).await;
        }

        // TODO: Implement other IQ handling (disco, ping, etc.)
        debug!("Received unhandled IQ stanza");
        Ok(())
    }

    /// Handle a MAM (Message Archive Management) query.
    ///
    /// Processes the query, retrieves archived messages from storage,
    /// and sends result messages followed by a fin IQ per XEP-0313.
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_mam_query(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => {
                warn!("MAM query received before JID bound");
                return Err(XmppError::not_authorized(Some(
                    "Session not established".to_string(),
                )));
            }
        };

        // Extract the room JID from the 'to' attribute of the IQ
        let room_jid = match &iq.to {
            Some(jid) => jid.to_string(),
            None => {
                warn!("MAM query missing 'to' attribute");
                return Err(XmppError::bad_request(Some(
                    "MAM query must specify target archive (to attribute)".to_string(),
                )));
            }
        };

        debug!(
            sender = %sender_jid,
            room = %room_jid,
            "Processing MAM query"
        );

        // Parse the MAM query
        let (query_id, mam_query) = parse_mam_query(&iq)?;

        debug!(
            query_id = %query_id,
            query = ?mam_query,
            "Parsed MAM query parameters"
        );

        // Execute the query against MAM storage
        let result = self
            .mam_storage
            .query_messages(&room_jid, &mam_query)
            .await
            .map_err(|e| {
                warn!(error = %e, "MAM query failed");
                XmppError::internal_server_error(Some(format!("MAM query failed: {}", e)))
            })?;

        debug!(
            message_count = result.messages.len(),
            complete = result.complete,
            "MAM query returned results"
        );

        // Build and send result messages for each archived message
        let result_messages = build_result_messages(&query_id, &sender_jid.to_string(), &result.messages);

        for msg in result_messages {
            self.stream.write_stanza(&Stanza::Message(msg)).await?;
        }

        // Send the fin IQ response
        let fin_iq = build_fin_iq(&iq, &result);
        self.stream.write_stanza(&Stanza::Iq(fin_iq)).await?;

        debug!(
            query_id = %query_id,
            messages_sent = result.messages.len(),
            "MAM query completed"
        );

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
