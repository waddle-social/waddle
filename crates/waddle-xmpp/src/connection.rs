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

use crate::disco::{
    build_disco_info_response, build_disco_items_response, is_disco_info_query,
    is_disco_items_query, muc_room_features, muc_service_features, parse_disco_info_query,
    parse_disco_items_query, server_features, DiscoItem, Feature, Identity,
};
use crate::mam::{
    add_stanza_id, build_fin_iq, build_result_messages, is_mam_query, parse_mam_query,
    ArchivedMessage, MamStorage,
};
use crate::stream_management::StreamManagementState;
use crate::metrics::{record_muc_occupant_count, record_muc_presence};
use crate::muc::{
    affiliation::{AffiliationResolver, AppStateAffiliationResolver},
    build_leave_presence, build_occupant_presence, parse_muc_presence, MucJoinRequest,
    MucLeaveRequest, MucMessage, MucPresenceAction, MucRoomRegistry,
};
use crate::types::{Affiliation, Role};
use crate::parser::ParsedStanza;
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
    /// XEP-0198 Stream Management state
    sm_state: StreamManagementState,
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
            sm_state: StreamManagementState::new(),
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
    /// This function handles:
    /// - Inbound stanzas from the client (messages, presence, IQs)
    /// - XEP-0198 Stream Management stanzas (enable, r, a)
    /// - Outbound stanzas routed from other connections via the registry
    async fn process_stanzas(&mut self) -> Result<(), XmppError> {
        // Take ownership of the outbound receiver
        let mut outbound_rx = self.outbound_rx.take();

        loop {
            tokio::select! {
                // Handle inbound stanzas from the client (using raw parser for SM support)
                inbound_result = self.stream.read_parsed_stanza() => {
                    match inbound_result {
                        Ok(Some(parsed)) => {
                            if let Err(e) = self.handle_parsed_stanza(parsed).await {
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
                            // Track outbound stanzas for SM acknowledgment
                            if self.sm_state.enabled {
                                self.sm_state.increment_outbound();
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

    /// Handle a raw parsed stanza, including SM stanzas.
    async fn handle_parsed_stanza(&mut self, parsed: ParsedStanza) -> Result<(), XmppError> {
        match parsed {
            ParsedStanza::StreamEnd => {
                debug!("Stream end received");
                Ok(())
            }
            ParsedStanza::Message(element) => {
                let msg = element.try_into()
                    .map_err(|e| XmppError::xml_parse(format!("Invalid message: {:?}", e)))?;
                // Increment inbound count for SM
                if self.sm_state.enabled {
                    self.sm_state.increment_inbound();
                }
                self.handle_stanza(Stanza::Message(msg)).await
            }
            ParsedStanza::Presence(element) => {
                let pres = element.try_into()
                    .map_err(|e| XmppError::xml_parse(format!("Invalid presence: {:?}", e)))?;
                // Increment inbound count for SM
                if self.sm_state.enabled {
                    self.sm_state.increment_inbound();
                }
                self.handle_stanza(Stanza::Presence(pres)).await
            }
            ParsedStanza::Iq(element) => {
                let iq = element.try_into()
                    .map_err(|e| XmppError::xml_parse(format!("Invalid iq: {:?}", e)))?;
                // Increment inbound count for SM
                if self.sm_state.enabled {
                    self.sm_state.increment_inbound();
                }
                self.handle_stanza(Stanza::Iq(iq)).await
            }
            // XEP-0198 Stream Management stanzas
            ParsedStanza::SmEnable { resume, max } => {
                self.handle_sm_enable(resume, max).await
            }
            ParsedStanza::SmRequest => {
                self.handle_sm_request().await
            }
            ParsedStanza::SmAck { h } => {
                self.handle_sm_ack(h).await
            }
            ParsedStanza::SmResume { previd, h } => {
                self.handle_sm_resume(&previd, h).await
            }
            _ => {
                debug!("Ignoring unexpected parsed stanza type");
                Ok(())
            }
        }
    }

    /// Handle XEP-0198 Stream Management enable request.
    async fn handle_sm_enable(&mut self, resume: bool, max: Option<u32>) -> Result<(), XmppError> {
        debug!(resume = resume, max = ?max, "Received SM enable request");

        // Generate a stream ID for potential resumption
        let stream_id = uuid::Uuid::new_v4().to_string();

        // Enable SM with or without resumption
        // For now, we support resumption if requested, with a max timeout of 5 minutes
        let max_seconds = if resume { Some(max.unwrap_or(300).min(300)) } else { None };

        self.sm_state.enable(stream_id.clone(), resume, max_seconds);

        // Send enabled response
        self.stream.send_sm_enabled(&stream_id, resume, max_seconds).await?;

        info!(
            stream_id = %stream_id,
            resume = resume,
            "Stream Management enabled"
        );

        Ok(())
    }

    /// Handle XEP-0198 Stream Management ack request (<r/>).
    async fn handle_sm_request(&mut self) -> Result<(), XmppError> {
        if !self.sm_state.enabled {
            debug!("SM request received but SM not enabled, ignoring");
            return Ok(());
        }

        let h = self.sm_state.get_inbound_count();
        debug!(h = h, "Sending SM ack in response to request");
        self.stream.send_sm_ack(h).await
    }

    /// Handle XEP-0198 Stream Management ack response (<a h='N'/>).
    async fn handle_sm_ack(&mut self, h: u32) -> Result<(), XmppError> {
        if !self.sm_state.enabled {
            debug!("SM ack received but SM not enabled, ignoring");
            return Ok(());
        }

        debug!(h = h, previous = self.sm_state.last_acked, "Received SM ack from client");
        self.sm_state.acknowledge(h);
        Ok(())
    }

    /// Handle XEP-0198 Stream Management resume request.
    ///
    /// Note: Full resumption requires storing session state across disconnections,
    /// which is not yet implemented. For now, we reject resume requests.
    async fn handle_sm_resume(&mut self, previd: &str, h: u32) -> Result<(), XmppError> {
        debug!(previd = %previd, h = h, "Received SM resume request");

        // For now, always reject resume requests since we don't persist session state
        self.stream.send_sm_failed(Some("item-not-found"), None).await?;

        warn!(previd = %previd, "SM resume rejected - session not found (resumption not yet implemented)");
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
        let sender_nick = room
            .find_nick_by_real_jid(&sender_jid)
            .ok_or_else(|| {
                debug!(
                    sender = %sender_jid,
                    room = %room_jid,
                    "Sender is not an occupant of the room"
                );
                XmppError::forbidden(Some(format!(
                    "You are not an occupant of {}",
                    room_jid
                )))
            })?
            .to_owned();

        // Broadcast the message to all occupants
        let mut outbound_messages = room.broadcast_message(&sender_nick, &muc_msg.message)?;

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

    /// Handle a presence stanza.
    ///
    /// Routes presence based on destination:
    /// - MUC presence (to room@muc.domain/nick): Join/leave room operations
    /// - Other presence: Currently logged and ignored
    #[instrument(skip(self, pres), fields(presence_type = ?pres.type_, to = ?pres.to))]
    async fn handle_presence(
        &mut self,
        pres: xmpp_parsers::presence::Presence,
    ) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => {
                warn!("Presence received before JID bound");
                return Err(XmppError::not_authorized(Some(
                    "Session not established".to_string(),
                )));
            }
        };

        // Parse the presence to see if it's a MUC action
        let muc_domain = self.room_registry.muc_domain();
        match parse_muc_presence(&pres, &sender_jid, muc_domain)? {
            MucPresenceAction::Join(join_req) => {
                self.handle_muc_join(join_req).await
            }
            MucPresenceAction::Leave(leave_req) => {
                self.handle_muc_leave(leave_req).await
            }
            MucPresenceAction::NotMuc => {
                // Regular presence, not MUC-related
                debug!("Received non-MUC presence stanza");
                Ok(())
            }
        }
    }

    /// Handle a MUC join request.
    ///
    /// Per XEP-0045:
    /// - Adds the user to the room as an occupant
    /// - Sends existing occupants' presence to the joining user
    /// - Sends the joining user's presence to all existing occupants
    /// - Sends self-presence with status code 110 to the joining user
    ///
    /// Affiliation is resolved from Zanzibar permissions:
    /// - "owner" permission -> Owner affiliation
    /// - "admin"/"moderator"/"manager" permission -> Admin affiliation
    /// - "member"/"writer"/"viewer" permission -> Member affiliation
    /// - No permission -> None affiliation (denied for members-only rooms)
    #[instrument(skip(self, join_req), fields(room = %join_req.room_jid, nick = %join_req.nick))]
    async fn handle_muc_join(&mut self, join_req: MucJoinRequest) -> Result<(), XmppError> {
        debug!(
            sender = %join_req.sender_jid,
            "Processing MUC join request"
        );

        // Check if this is a MUC room we manage
        if !self.room_registry.is_muc_jid(&join_req.room_jid) {
            return Err(XmppError::item_not_found(Some(format!(
                "Room {} not found",
                join_req.room_jid
            ))));
        }

        // Get or create the room data
        let room_data = self.room_registry.get_room_data(&join_req.room_jid).ok_or_else(|| {
            XmppError::item_not_found(Some(format!("Room {} not found", join_req.room_jid)))
        })?;

        // Get the user's DID from the session for permission checking
        let user_did = self
            .session
            .as_ref()
            .map(|s| s.did.clone())
            .unwrap_or_else(|| {
                // Fallback: extract DID-like identifier from JID if no session
                // This handles edge cases in testing scenarios
                join_req.sender_jid.to_bare().to_string()
            });

        // Lock the room for modification
        let mut room = room_data.write().await;

        // Get room metadata needed for permission resolution
        let waddle_id = room.waddle_id.clone();
        let channel_id = room.channel_id.clone();
        let is_members_only = room.config.members_only;

        // Resolve affiliation from Zanzibar permissions before checking join permissions
        let resolver = AppStateAffiliationResolver::new(
            Arc::clone(&self.app_state),
            self.domain.clone(),
        );

        let resolved_affiliation = match resolver
            .resolve_affiliation(&user_did, &waddle_id, &channel_id)
            .await
        {
            Ok(affiliation) => {
                debug!(
                    user = %user_did,
                    affiliation = %affiliation,
                    "Resolved affiliation from Zanzibar permissions"
                );
                affiliation
            }
            Err(e) => {
                // Permission check failed - handle gracefully
                warn!(
                    user = %user_did,
                    error = %e,
                    "Failed to resolve affiliation from Zanzibar, using default"
                );
                // For open rooms, default to None affiliation (can still join)
                // For members-only rooms, this will be denied below
                Affiliation::None
            }
        };

        // Update the room's affiliation list with the resolved affiliation
        // This ensures the affiliation is persisted for subsequent queries
        let bare_jid = join_req.sender_jid.to_bare();
        if resolved_affiliation != Affiliation::None {
            room.update_affiliation_from_resolver(bare_jid.clone(), resolved_affiliation);
            debug!(
                jid = %bare_jid,
                affiliation = %resolved_affiliation,
                "Updated room affiliation list from Zanzibar"
            );
        }

        // Check if user is allowed to join (now uses the updated affiliation list)
        if !room.can_user_join(&bare_jid) {
            // For members-only rooms, users without membership are denied
            if is_members_only {
                return Err(XmppError::registration_required(Some(format!(
                    "Room {} is members-only and you do not have membership",
                    join_req.room_jid
                ))));
            }
            return Err(XmppError::forbidden(Some(format!(
                "You are not allowed to join {}",
                join_req.room_jid
            ))));
        }

        // Check if room is full
        if room.is_full() {
            return Err(XmppError::service_unavailable(Some(
                "Room is full".to_string(),
            )));
        }

        // Check if nick is already taken by another user
        if let Some(existing) = room.get_occupant(&join_req.nick) {
            if existing.real_jid != join_req.sender_jid {
                return Err(XmppError::conflict(Some(format!(
                    "Nickname {} is already in use",
                    join_req.nick
                ))));
            }
            // User is already in the room with this nick - treat as presence refresh
            debug!("User already in room, refreshing presence");
        }

        // Get existing occupants before adding the new one (for presence broadcast)
        let existing_occupants: Vec<(FullJid, String, Affiliation, Role)> = room
            .occupants
            .values()
            .map(|o| (o.real_jid.clone(), o.nick.clone(), o.affiliation, o.role))
            .collect();

        // Add the occupant to the room (uses affiliation from the updated list)
        let new_occupant = room.add_occupant_with_affiliation(join_req.sender_jid.clone(), join_req.nick.clone());
        let new_occupant_affiliation = new_occupant.affiliation;
        let new_occupant_role = new_occupant.role;

        let occupant_count = room.occupant_count();

        // Build the new occupant's room JID
        let new_occupant_room_jid = room
            .room_jid
            .clone()
            .with_resource_str(&join_req.nick)
            .map_err(|e| XmppError::internal(format!("Invalid nick as resource: {}", e)))?;

        drop(room); // Release the write lock

        // Record metrics
        record_muc_presence("join", &join_req.room_jid.to_string());
        record_muc_occupant_count(occupant_count as i64, &join_req.room_jid.to_string());

        // Send existing occupants' presence to the joining user
        for (existing_jid, existing_nick, existing_affiliation, existing_role) in &existing_occupants {
            let existing_room_jid = join_req
                .room_jid
                .clone()
                .with_resource_str(existing_nick)
                .map_err(|e| XmppError::internal(format!("Invalid nick as resource: {}", e)))?;

            let presence = build_occupant_presence(
                &existing_room_jid,
                &join_req.sender_jid,
                *existing_affiliation,
                *existing_role,
                false, // not self
                Some(existing_jid), // real JID for semi-anonymous rooms
            );

            self.stream.write_stanza(&Stanza::Presence(presence)).await?;
        }

        // Send the new occupant's presence to all existing occupants
        for (existing_jid, _, _, _) in &existing_occupants {
            let presence = build_occupant_presence(
                &new_occupant_room_jid,
                existing_jid,
                new_occupant_affiliation,
                new_occupant_role,
                false, // not self
                Some(&join_req.sender_jid),
            );

            let stanza = Stanza::Presence(presence);
            let _ = self.connection_registry.send_to(existing_jid, stanza).await;
        }

        // Send self-presence to the joining user (with status code 110)
        let self_presence = build_occupant_presence(
            &new_occupant_room_jid,
            &join_req.sender_jid,
            new_occupant_affiliation,
            new_occupant_role,
            true, // is_self - includes status code 110
            Some(&join_req.sender_jid),
        );

        self.stream.write_stanza(&Stanza::Presence(self_presence)).await?;

        info!(
            room = %join_req.room_jid,
            nick = %join_req.nick,
            occupant_count = occupant_count,
            "User joined MUC room"
        );

        Ok(())
    }

    /// Handle a MUC leave request.
    ///
    /// Per XEP-0045:
    /// - Removes the user from the room
    /// - Sends unavailable presence to all remaining occupants
    /// - Sends self-presence unavailable with status code 110 to the leaving user
    #[instrument(skip(self, leave_req), fields(room = %leave_req.room_jid, nick = %leave_req.nick))]
    async fn handle_muc_leave(&mut self, leave_req: MucLeaveRequest) -> Result<(), XmppError> {
        debug!(
            sender = %leave_req.sender_jid,
            "Processing MUC leave request"
        );

        // Get the room data
        let room_data = self.room_registry.get_room_data(&leave_req.room_jid).ok_or_else(|| {
            XmppError::item_not_found(Some(format!("Room {} not found", leave_req.room_jid)))
        })?;

        // Lock the room for modification
        let mut room = room_data.write().await;

        // Find the occupant by their real JID (not by nick from the presence, as that could be manipulated)
        let occupant_nick = room
            .find_nick_by_real_jid(&leave_req.sender_jid)
            .map(|s| s.to_owned());

        let nick = match occupant_nick {
            Some(n) => n,
            None => {
                debug!("User not in room, ignoring leave");
                return Ok(());
            }
        };

        // Get the occupant's info before removal
        let occupant = room.get_occupant(&nick).ok_or_else(|| {
            XmppError::internal("Occupant disappeared during leave".to_string())
        })?;
        let affiliation = occupant.affiliation;

        // Build the leaving user's room JID
        let leaving_room_jid = room
            .room_jid
            .clone()
            .with_resource_str(&nick)
            .map_err(|e| XmppError::internal(format!("Invalid nick as resource: {}", e)))?;

        // Get remaining occupants (excluding the one leaving)
        let remaining_occupants: Vec<FullJid> = room
            .occupants
            .values()
            .filter(|o| o.real_jid != leave_req.sender_jid)
            .map(|o| o.real_jid.clone())
            .collect();

        // Remove the occupant
        room.remove_occupant(&nick);
        let occupant_count = room.occupant_count();

        drop(room); // Release the write lock

        // Record metrics
        record_muc_presence("leave", &leave_req.room_jid.to_string());
        record_muc_occupant_count(occupant_count as i64, &leave_req.room_jid.to_string());

        // Send unavailable presence to all remaining occupants
        for occupant_jid in &remaining_occupants {
            let presence = build_leave_presence(
                &leaving_room_jid,
                occupant_jid,
                affiliation,
                false, // not self
            );

            let stanza = Stanza::Presence(presence);
            let _ = self.connection_registry.send_to(occupant_jid, stanza).await;
        }

        // Send self-presence unavailable to the leaving user
        let self_presence = build_leave_presence(
            &leaving_room_jid,
            &leave_req.sender_jid,
            affiliation,
            true, // is_self - includes status code 110
        );

        self.stream.write_stanza(&Stanza::Presence(self_presence)).await?;

        info!(
            room = %leave_req.room_jid,
            nick = %nick,
            occupant_count = occupant_count,
            "User left MUC room"
        );

        Ok(())
    }

    /// Handle an IQ stanza.
    ///
    /// Currently supports:
    /// - disco#info queries (XEP-0030)
    /// - disco#items queries (XEP-0030)
    /// - MAM queries (XEP-0313)
    #[instrument(skip(self, iq), fields(iq_type = ?iq.payload, iq_id = %iq.id))]
    async fn handle_iq(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        // Check if this is a disco#info query
        if is_disco_info_query(&iq) {
            return self.handle_disco_info_query(iq).await;
        }

        // Check if this is a disco#items query
        if is_disco_items_query(&iq) {
            return self.handle_disco_items_query(iq).await;
        }

        // Check if this is a MAM query
        if is_mam_query(&iq) {
            return self.handle_mam_query(iq).await;
        }

        // Unhandled IQ - log and continue
        debug!("Received unhandled IQ stanza");
        Ok(())
    }

    /// Handle a disco#info query.
    ///
    /// Returns identity and supported features for:
    /// - Server domain: Server identity + server features
    /// - MUC domain: Conference service identity + MUC features
    /// - MUC room: Conference room identity + room features
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_disco_info_query(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        let query = parse_disco_info_query(&iq)?;

        let muc_domain = self.room_registry.muc_domain();

        // Determine what entity is being queried
        let (identities, features) = match query.target.as_deref() {
            // Query to server domain
            Some(target) if target == self.domain => {
                debug!(domain = %self.domain, "disco#info query to server domain");
                (
                    vec![Identity::server(Some("Waddle XMPP Server"))],
                    server_features(),
                )
            }
            // Query to MUC domain
            Some(target) if target == muc_domain => {
                debug!(domain = %muc_domain, "disco#info query to MUC domain");
                (
                    vec![Identity::muc_service(Some("Multi-User Chat"))],
                    muc_service_features(),
                )
            }
            // Query to MUC room
            Some(target) if target.ends_with(&format!("@{}", muc_domain)) => {
                let room_jid: jid::BareJid = target
                    .parse()
                    .map_err(|e| XmppError::bad_request(Some(format!("Invalid JID: {}", e))))?;

                if let Some(room_data) = self.room_registry.get_room_data(&room_jid) {
                    let room = room_data.read().await;
                    debug!(room = %room_jid, "disco#info query to MUC room");
                    (
                        vec![Identity::muc_room(Some(&room.config.name))],
                        muc_room_features(
                            room.config.persistent,
                            room.config.members_only,
                            room.config.moderated,
                        ),
                    )
                } else {
                    // Room doesn't exist
                    return Err(XmppError::item_not_found(Some(format!(
                        "Room {} not found",
                        room_jid
                    ))));
                }
            }
            // No target or unknown target - default to server
            None | Some(_) => {
                debug!(target = ?query.target, "disco#info query (defaulting to server)");
                (
                    vec![Identity::server(Some("Waddle XMPP Server"))],
                    server_features(),
                )
            }
        };

        let response = build_disco_info_response(&iq, &identities, &features, query.node.as_deref());
        self.stream.write_stanza(&Stanza::Iq(response)).await?;

        debug!("Sent disco#info response");
        Ok(())
    }

    /// Handle a disco#items query.
    ///
    /// Returns available items/services:
    /// - Server domain: Returns MUC service component
    /// - MUC domain: Returns list of available rooms
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_disco_items_query(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        let query = parse_disco_items_query(&iq)?;

        let muc_domain = self.room_registry.muc_domain();

        // Determine what entity is being queried
        let items = match query.target.as_deref() {
            // Query to server domain - return MUC service
            Some(target) if target == self.domain => {
                debug!(domain = %self.domain, "disco#items query to server domain");
                vec![DiscoItem::muc_service(muc_domain, Some("Multi-User Chat"))]
            }
            // Query to MUC domain - return room list
            Some(target) if target == muc_domain => {
                debug!(domain = %muc_domain, "disco#items query to MUC domain");
                let room_infos = self.room_registry.list_room_info().await;
                room_infos
                    .iter()
                    .map(|info| DiscoItem::muc_room(&info.room_jid.to_string(), &info.name))
                    .collect()
            }
            // Query to MUC room - return empty list (no sub-items)
            Some(target) if target.ends_with(&format!("@{}", muc_domain)) => {
                debug!(room = %target, "disco#items query to MUC room");
                vec![] // Rooms don't have sub-items
            }
            // No target or unknown target - default to server services
            None | Some(_) => {
                debug!(target = ?query.target, "disco#items query (defaulting to server)");
                vec![DiscoItem::muc_service(muc_domain, Some("Multi-User Chat"))]
            }
        };

        let response = build_disco_items_response(&iq, &items, query.node.as_deref());
        self.stream.write_stanza(&Stanza::Iq(response)).await?;

        debug!(item_count = items.len(), "Sent disco#items response");
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
