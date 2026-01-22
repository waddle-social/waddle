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

use crate::carbons::{
    build_carbons_result, build_received_carbon, build_sent_carbon, is_carbons_disable,
    is_carbons_enable, should_copy_message,
};
use crate::roster::{
    build_roster_push, build_roster_result, build_roster_result_empty, is_roster_get,
    is_roster_set, parse_roster_get, parse_roster_set, RosterItem,
};
use crate::presence::{
    build_available_presence, build_subscription_presence, build_unavailable_presence,
    parse_subscription_presence, PresenceAction, PresenceSubscriptionRequest,
    SubscriptionStateMachine, SubscriptionType,
};
use crate::disco::{
    build_disco_info_response, build_disco_items_response, is_disco_info_query,
    is_disco_items_query, muc_room_features, muc_service_features, parse_disco_info_query,
    parse_disco_items_query, server_features, DiscoItem, Identity,
};
use crate::isr::{build_isr_token_error, build_isr_token_result, is_isr_token_request, SharedIsrTokenStore};
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
use crate::stream::{PreAuthResult, SaslAuthResult, XmppStream};
use crate::types::ConnectionState;
use crate::xep::xep0054::{
    is_vcard_get, is_vcard_set, parse_vcard_from_iq, build_vcard_response,
    build_empty_vcard_response, build_vcard_success,
};
use crate::xep::xep0077::RegistrationError;
use crate::xep::xep0363::{
    is_upload_request, parse_upload_request, build_upload_slot_response, build_upload_error,
    UploadSlot, UploadError,
};
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
    /// XEP-0280 Message Carbons enabled state
    carbons_enabled: bool,
    /// XEP-0397 ISR token store for instant stream resumption
    isr_token_store: SharedIsrTokenStore,
    /// Current ISR token for this connection (if any)
    current_isr_token: Option<String>,
}

impl<S: AppState, M: MamStorage> ConnectionActor<S, M> {
    /// Handle a new incoming connection.
    #[instrument(
        name = "xmpp.connection.handle",
        skip(tcp_stream, tls_acceptor, app_state, room_registry, connection_registry, mam_storage, isr_token_store),
        fields(peer = %peer_addr)
    )]
    #[allow(clippy::too_many_arguments)]
    pub async fn handle_connection(
        tcp_stream: TcpStream,
        peer_addr: SocketAddr,
        tls_acceptor: TlsAcceptor,
        domain: String,
        app_state: Arc<S>,
        room_registry: Arc<MucRoomRegistry>,
        connection_registry: Arc<ConnectionRegistry>,
        mam_storage: Arc<M>,
        isr_token_store: SharedIsrTokenStore,
        registration_enabled: bool,
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
            carbons_enabled: false,
            isr_token_store,
            current_isr_token: None,
        };

        actor.run(tls_acceptor, registration_enabled).await
    }

    /// Main connection loop.
    async fn run(&mut self, tls_acceptor: TlsAcceptor, registration_enabled: bool) -> Result<(), XmppError> {
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

        // TLS established, send new features (SASL with optional registration)
        self.state = ConnectionState::TlsEstablished;
        let _header = self.stream.read_stream_header().await?;

        // Enable XEP-0077 In-Band Registration in stream features if configured
        self.stream.send_features_sasl_with_registration(registration_enabled).await?;

        // Handle pre-auth phase (registration IQs or SASL authentication)
        self.state = ConnectionState::Authenticating;
        let (jid, session) = self.handle_pre_auth_phase(registration_enabled).await?;
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

    /// Handle SASL authentication, supporting both PLAIN and OAUTHBEARER.
    ///
    /// For OAUTHBEARER, this may involve multiple rounds:
    /// 1. Client sends empty OAUTHBEARER → Server sends discovery URL
    /// 2. Client completes OAuth flow externally
    /// 3. Client sends OAUTHBEARER with token → Server validates and succeeds
    ///
    /// After successful authentication, sends SASL success with an ISR token
    /// per XEP-0397 for instant stream resumption support.
    /// On validation failure, sends SASL failure with not-authorized condition.
    #[instrument(skip(self), name = "xmpp.connection.sasl_auth")]
    async fn handle_sasl_authentication(&mut self) -> Result<(jid::BareJid, Session), XmppError> {
        loop {
            let auth_result = self.stream.handle_sasl_auth().await?;

            match auth_result {
                SaslAuthResult::Plain { jid, token } => {
                    // PLAIN: Validate session with JID and token
                    match self
                        .app_state
                        .validate_session(&jid.clone().into(), &token)
                        .await
                    {
                        Ok(session) => {
                            // Send SASL success with ISR token
                            self.send_sasl_success_with_isr(&session, &jid).await?;
                            return Ok((jid, session));
                        }
                        Err(e) => {
                            // Send SASL failure before returning error
                            warn!(error = %e, jid = %jid, "PLAIN authentication failed");
                            self.stream.send_sasl_failure("not-authorized").await?;
                            return Err(e);
                        }
                    }
                }
                SaslAuthResult::OAuthBearer { token, authzid: _ } => {
                    // OAUTHBEARER: Validate session using just the token
                    // The token is the session ID which we can look up directly
                    match self.app_state.validate_session_token(&token).await {
                        Ok(session) => {
                            // Derive JID from session
                            let jid = session.jid.clone();

                            // Send SASL success with ISR token
                            self.send_sasl_success_with_isr(&session, &jid).await?;
                            return Ok((jid, session));
                        }
                        Err(e) => {
                            // Send SASL failure before returning error
                            warn!(error = %e, "OAUTHBEARER authentication failed");
                            self.stream.send_sasl_failure("not-authorized").await?;
                            return Err(e);
                        }
                    }
                }
                SaslAuthResult::OAuthBearerDiscovery => {
                    // Client requested OAuth discovery - send discovery URL
                    let discovery_url = self.app_state.oauth_discovery_url();
                    self.stream.send_oauthbearer_discovery(&discovery_url).await?;

                    debug!(discovery_url = %discovery_url, "Sent OAUTHBEARER discovery, waiting for client to complete OAuth");

                    // Client will disconnect and reconnect with token after OAuth
                    // Or some clients may send the token in the same session
                    // Continue the loop to wait for the next auth attempt
                }
                SaslAuthResult::ScramSha256Challenge {
                    username,
                    server_first_message_b64,
                    scram_server,
                } => {
                    // SCRAM-SHA-256: Client sent client-first-message
                    // Look up SCRAM credentials for the username
                    match self.app_state.lookup_scram_credentials(&username).await {
                        Ok(Some(creds)) => {
                            // Send the challenge
                            self.stream.send_scram_challenge(&server_first_message_b64).await?;

                            // Continue the SCRAM exchange
                            match self.stream.continue_scram_auth(
                                scram_server,
                                &creds.stored_key,
                                &creds.server_key,
                            ).await {
                                Ok(SaslAuthResult::ScramSha256Complete { username }) => {
                                    // Authentication successful - create session for native user
                                    // The JID is username@domain
                                    let jid: jid::BareJid = format!("{}@{}", username, self.domain)
                                        .parse()
                                        .map_err(|e| XmppError::auth_failed(format!("Invalid JID: {}", e)))?;

                                    // For native users, the DID is the JID itself (no ATProto)
                                    let session = Session {
                                        did: jid.to_string(),
                                        jid: jid.clone(),
                                        created_at: Utc::now(),
                                        expires_at: Utc::now() + chrono::Duration::hours(24),
                                    };

                                    // Send SASL success with ISR token
                                    self.send_sasl_success_with_isr(&session, &jid).await?;
                                    return Ok((jid, session));
                                }
                                Ok(_) => {
                                    // Unexpected result
                                    warn!("Unexpected SCRAM auth result");
                                    self.stream.send_sasl_failure("not-authorized").await?;
                                    return Err(XmppError::auth_failed("SCRAM authentication failed"));
                                }
                                Err(e) => {
                                    warn!(error = %e, username = %username, "SCRAM-SHA-256 authentication failed");
                                    self.stream.send_sasl_failure("not-authorized").await?;
                                    return Err(e);
                                }
                            }
                        }
                        Ok(None) => {
                            // User not found - native JID auth may not be supported
                            warn!(username = %username, "SCRAM user not found");
                            self.stream.send_sasl_failure("not-authorized").await?;
                            return Err(XmppError::auth_failed("User not found"));
                        }
                        Err(e) => {
                            warn!(error = %e, username = %username, "Failed to lookup SCRAM credentials");
                            self.stream.send_sasl_failure("not-authorized").await?;
                            return Err(e);
                        }
                    }
                }
                SaslAuthResult::ScramSha256Complete { username } => {
                    // This variant should only be returned from continue_scram_auth,
                    // not from handle_sasl_auth directly. If we get here, something is wrong.
                    warn!(username = %username, "Unexpected ScramSha256Complete in main auth loop");
                    return Err(XmppError::internal("Unexpected SCRAM state".to_string()));
                }
            }
        }
    }

    /// Handle the pre-authentication phase with optional XEP-0077 registration.
    ///
    /// When registration is enabled, this handles the pre-auth loop where clients
    /// can either:
    /// 1. Send registration IQs to create an account (XEP-0077)
    /// 2. Send SASL auth to authenticate
    ///
    /// After successful registration, the connection continues and the client
    /// can authenticate with their new credentials.
    #[instrument(skip(self), name = "xmpp.connection.pre_auth")]
    async fn handle_pre_auth_phase(
        &mut self,
        registration_enabled: bool,
    ) -> Result<(jid::BareJid, Session), XmppError> {
        if !registration_enabled {
            // Registration not enabled, go directly to SASL authentication
            return self.handle_sasl_authentication().await;
        }

        // Pre-auth loop: handle registration IQs or SASL auth
        loop {
            let pre_auth_result = self.stream.read_pre_auth_stanza().await?;

            match pre_auth_result {
                PreAuthResult::SaslAuth(sasl_result) => {
                    // Client is attempting SASL authentication
                    // Process it using the existing SASL handler logic
                    return self.process_sasl_auth_result(sasl_result).await;
                }
                PreAuthResult::RegistrationIq { id, request } => {
                    // Handle registration IQ
                    match request {
                        None => {
                            // This is a 'get' request - send registration form
                            debug!(id = %id, "Sending registration form");
                            self.stream
                                .send_registration_form(&id, Some("Choose a username and password to register."))
                                .await?;
                            // Continue loop to wait for next stanza
                        }
                        Some(reg_request) => {
                            // This is a 'set' request - attempt to register
                            debug!(
                                id = %id,
                                username = %reg_request.username,
                                "Processing registration request"
                            );

                            match self.handle_registration_request(&id, reg_request).await {
                                Ok(()) => {
                                    // Registration successful - continue loop
                                    // Client should now send SASL auth with new credentials
                                    debug!(id = %id, "Registration successful, waiting for SASL auth");
                                }
                                Err(reg_error) => {
                                    // Send error response but continue loop
                                    self.stream.send_registration_error(&id, &reg_error).await?;
                                    debug!(id = %id, error = %reg_error, "Registration failed");
                                }
                            }
                            // Continue loop to wait for next stanza
                        }
                    }
                }
            }
        }
    }

    /// Process a SASL authentication result.
    ///
    /// This contains the logic extracted from `handle_sasl_authentication` to handle
    /// a single SASL auth result. Used by both direct SASL auth and pre-auth phase.
    async fn process_sasl_auth_result(
        &mut self,
        sasl_result: SaslAuthResult,
    ) -> Result<(jid::BareJid, Session), XmppError> {
        match sasl_result {
            SaslAuthResult::Plain { jid, token } => {
                // PLAIN: Validate session with JID and token
                match self
                    .app_state
                    .validate_session(&jid.clone().into(), &token)
                    .await
                {
                    Ok(session) => {
                        // Send SASL success with ISR token
                        self.send_sasl_success_with_isr(&session, &jid).await?;
                        Ok((jid, session))
                    }
                    Err(e) => {
                        // Send SASL failure before returning error
                        warn!(error = %e, jid = %jid, "PLAIN authentication failed");
                        self.stream.send_sasl_failure("not-authorized").await?;
                        Err(e)
                    }
                }
            }
            SaslAuthResult::OAuthBearer { token, authzid: _ } => {
                // OAUTHBEARER: Validate session using just the token
                match self.app_state.validate_session_token(&token).await {
                    Ok(session) => {
                        let jid = session.jid.clone();
                        self.send_sasl_success_with_isr(&session, &jid).await?;
                        Ok((jid, session))
                    }
                    Err(e) => {
                        warn!(error = %e, "OAUTHBEARER authentication failed");
                        self.stream.send_sasl_failure("not-authorized").await?;
                        Err(e)
                    }
                }
            }
            SaslAuthResult::OAuthBearerDiscovery => {
                // Client requested OAuth discovery - send discovery URL
                let discovery_url = self.app_state.oauth_discovery_url();
                self.stream.send_oauthbearer_discovery(&discovery_url).await?;
                debug!(discovery_url = %discovery_url, "Sent OAUTHBEARER discovery");
                // Need to wait for next auth attempt - this would need to loop
                // For now, return an error indicating client should reconnect
                Err(XmppError::auth_failed("OAuth discovery sent - complete OAuth flow and reconnect"))
            }
            SaslAuthResult::ScramSha256Challenge {
                username,
                server_first_message_b64,
                scram_server,
            } => {
                // SCRAM-SHA-256: Look up credentials and continue
                match self.app_state.lookup_scram_credentials(&username).await {
                    Ok(Some(creds)) => {
                        self.stream.send_scram_challenge(&server_first_message_b64).await?;
                        match self.stream.continue_scram_auth(
                            scram_server,
                            &creds.stored_key,
                            &creds.server_key,
                        ).await {
                            Ok(SaslAuthResult::ScramSha256Complete { username }) => {
                                let jid: jid::BareJid = format!("{}@{}", username, self.domain)
                                    .parse()
                                    .map_err(|e| XmppError::auth_failed(format!("Invalid JID: {}", e)))?;
                                let session = Session {
                                    did: jid.to_string(),
                                    jid: jid.clone(),
                                    created_at: Utc::now(),
                                    expires_at: Utc::now() + chrono::Duration::hours(24),
                                };
                                self.send_sasl_success_with_isr(&session, &jid).await?;
                                Ok((jid, session))
                            }
                            Ok(_) => {
                                warn!("Unexpected SCRAM auth result");
                                self.stream.send_sasl_failure("not-authorized").await?;
                                Err(XmppError::auth_failed("SCRAM authentication failed"))
                            }
                            Err(e) => {
                                warn!(error = %e, username = %username, "SCRAM-SHA-256 authentication failed");
                                self.stream.send_sasl_failure("not-authorized").await?;
                                Err(e)
                            }
                        }
                    }
                    Ok(None) => {
                        warn!(username = %username, "SCRAM user not found");
                        self.stream.send_sasl_failure("not-authorized").await?;
                        Err(XmppError::auth_failed("User not found"))
                    }
                    Err(e) => {
                        warn!(error = %e, username = %username, "Failed to lookup SCRAM credentials");
                        self.stream.send_sasl_failure("not-authorized").await?;
                        Err(e)
                    }
                }
            }
            SaslAuthResult::ScramSha256Complete { username } => {
                warn!(username = %username, "Unexpected ScramSha256Complete");
                Err(XmppError::internal("Unexpected SCRAM state".to_string()))
            }
        }
    }

    /// Handle a XEP-0077 registration request.
    ///
    /// Validates the registration data and creates the user via AppState.
    async fn handle_registration_request(
        &mut self,
        id: &str,
        request: crate::xep::xep0077::RegistrationRequest,
    ) -> Result<(), RegistrationError> {
        // Validate password strength (basic check)
        if request.password.len() < 6 {
            return Err(RegistrationError::NotAcceptable(
                "Password must be at least 6 characters".to_string(),
            ));
        }

        // Check if user already exists
        match self.app_state.native_user_exists(&request.username).await {
            Ok(true) => {
                return Err(RegistrationError::Conflict);
            }
            Ok(false) => {
                // User doesn't exist, proceed with registration
            }
            Err(e) => {
                warn!(error = %e, "Failed to check user existence");
                return Err(RegistrationError::InternalError(e.to_string()));
            }
        }

        // Register the user
        match self
            .app_state
            .register_native_user(
                &request.username,
                &request.password,
                request.email.as_deref(),
            )
            .await
        {
            Ok(()) => {
                info!(
                    username = %request.username,
                    "User registered via XEP-0077"
                );

                // Send success response
                self.stream.send_registration_success(id).await.map_err(|e| {
                    RegistrationError::InternalError(format!("Failed to send success: {}", e))
                })?;

                Ok(())
            }
            Err(e) => {
                // Map XmppError to RegistrationError
                if e.to_string().contains("already exists") || e.to_string().contains("conflict") {
                    Err(RegistrationError::Conflict)
                } else if e.to_string().contains("not acceptable") || e.to_string().contains("invalid") {
                    Err(RegistrationError::NotAcceptable(e.to_string()))
                } else {
                    Err(RegistrationError::InternalError(e.to_string()))
                }
            }
        }
    }

    /// Send SASL success response with an ISR resumption token.
    ///
    /// Creates a new ISR token for the session and sends it in the SASL success response.
    async fn send_sasl_success_with_isr(
        &mut self,
        session: &Session,
        jid: &jid::BareJid,
    ) -> Result<(), XmppError> {
        // Create an ISR token for this session
        let isr_token = self.isr_token_store.create_token(
            session.did.clone(),
            jid.clone(),
        );

        // Store the token ID for this connection
        self.current_isr_token = Some(isr_token.token.clone());

        // Send success with ISR token
        self.stream.send_sasl_success_with_isr(&isr_token.to_xml()).await?;

        debug!(
            did = %session.did,
            jid = %jid,
            token_expiry = %isr_token.expiry,
            "Created ISR token for session"
        );

        Ok(())
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

        // Update the ISR token with SM stream ID for instant resumption
        if let Some(ref token) = self.current_isr_token {
            if let Some(session) = &self.session {
                if let Some(jid) = &self.jid {
                    // Create a new ISR token with SM state
                    let new_isr_token = self.isr_token_store.create_token_with_sm(
                        session.did.clone(),
                        jid.to_bare(),
                        stream_id.clone(),
                        self.sm_state.inbound_count,
                        self.sm_state.outbound_count,
                    );

                    // Remove old token and store new one
                    self.isr_token_store.consume_token(token);
                    self.current_isr_token = Some(new_isr_token.token.clone());

                    debug!(
                        stream_id = %stream_id,
                        isr_token = %&new_isr_token.token[..new_isr_token.token.len().min(8)],
                        "Updated ISR token with SM state"
                    );
                }
            }
        }

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
    /// Supports two modes of resumption:
    /// 1. ISR token-based (XEP-0397): The previd is an ISR token from SASL success
    /// 2. Standard SM (XEP-0198): The previd is the SM stream ID
    ///
    /// ISR resumption is tried first since it includes full session state.
    async fn handle_sm_resume(&mut self, previd: &str, h: u32) -> Result<(), XmppError> {
        debug!(previd = %previd, h = h, "Received SM resume request");

        // First, try ISR token-based resumption (XEP-0397)
        if let Some(isr_token) = self.isr_token_store.validate_token(previd) {
            info!(
                did = %isr_token.did,
                jid = %isr_token.jid,
                sm_stream_id = ?isr_token.sm_stream_id,
                "ISR token validated for instant resumption"
            );

            // Consume the token to prevent reuse
            self.isr_token_store.consume_token(previd);

            // Restore session state from the ISR token
            self.session = Some(Session {
                did: isr_token.did.clone(),
                jid: isr_token.jid.clone(),
                // Use session expiry from somewhere - for now use a long-lived session
                // In production, this should be looked up from the session store
                created_at: Utc::now(),
                expires_at: Utc::now() + chrono::Duration::hours(24),
            });

            // Restore SM state
            if let Some(sm_stream_id) = &isr_token.sm_stream_id {
                self.sm_state.enable(sm_stream_id.clone(), true, Some(300));
                // Restore counters - client's h tells us what they've received
                // Token's outbound count tells us what we sent before disconnect
                debug!(
                    client_h = h,
                    server_outbound = isr_token.sm_outbound_count,
                    server_inbound = isr_token.sm_inbound_count,
                    "Restoring SM counters"
                );
            }

            // Create a new ISR token for the resumed session
            let new_isr_token = if let Some(ref sm_id) = isr_token.sm_stream_id {
                self.isr_token_store.create_token_with_sm(
                    isr_token.did.clone(),
                    isr_token.jid.clone(),
                    sm_id.clone(),
                    isr_token.sm_inbound_count,
                    isr_token.sm_outbound_count,
                )
            } else {
                self.isr_token_store.create_token(
                    isr_token.did.clone(),
                    isr_token.jid.clone(),
                )
            };

            self.current_isr_token = Some(new_isr_token.token.clone());

            // Send resumed response with new ISR token
            self.stream
                .send_sm_resumed_with_isr(
                    isr_token.sm_stream_id.as_deref().unwrap_or(previd),
                    isr_token.sm_inbound_count,
                    &new_isr_token.to_xml(),
                )
                .await?;

            info!(
                jid = %isr_token.jid,
                sm_stream_id = ?isr_token.sm_stream_id,
                "Stream resumed via ISR token"
            );

            return Ok(());
        }

        // Standard SM resumption is not yet implemented (requires persistent session state)
        // For now, reject resume requests that aren't ISR tokens
        self.stream.send_sm_failed(Some("item-not-found"), None).await?;

        warn!(previd = %previd, "SM resume rejected - session not found (non-ISR resumption not yet implemented)");
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
                self.handle_chat_message(msg, sender_jid).await
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

    /// Handle a direct chat (1-to-1) message.
    ///
    /// Routes the message to the recipient's connected resources and handles
    /// XEP-0280 Message Carbons for synchronization across devices.
    ///
    /// Per XEP-0280:
    /// - After sending, "sent" carbons are delivered to sender's other clients
    /// - When delivering to recipient, "received" carbons go to recipient's other clients
    /// - Messages with `<private/>` or `<no-copy/>` are not carbon-copied
    #[instrument(skip(self, msg), fields(to = ?msg.to))]
    async fn handle_chat_message(
        &mut self,
        msg: xmpp_parsers::message::Message,
        sender_jid: FullJid,
    ) -> Result<(), XmppError> {
        let recipient_jid = match &msg.to {
            Some(jid) => jid.clone(),
            None => {
                warn!("Chat message missing 'to' attribute");
                return Err(XmppError::bad_request(Some(
                    "Message must have a recipient".to_string(),
                )));
            }
        };

        debug!(
            sender = %sender_jid,
            recipient = %recipient_jid,
            "Routing chat message"
        );

        // Ensure the message has the sender's full JID
        let mut msg_with_from = msg.clone();
        msg_with_from.from = Some(sender_jid.clone().into());

        // Determine if this message should be carbon-copied
        let should_carbon = should_copy_message(&msg);

        // Route to all connected resources for the recipient
        let recipient_bare = recipient_jid.to_bare();
        let recipient_resources = self.connection_registry.get_resources_for_user(&recipient_bare);

        let mut delivered = false;

        if recipient_resources.is_empty() {
            debug!(
                recipient = %recipient_bare,
                "Recipient has no connected resources"
            );
            // In a full implementation, we might queue for offline delivery
        } else {
            for resource_jid in &recipient_resources {
                let stanza = Stanza::Message(msg_with_from.clone());
                let result = self.connection_registry.send_to(resource_jid, stanza).await;

                match result {
                    SendResult::Sent => {
                        debug!(to = %resource_jid, "Message delivered to recipient resource");
                        delivered = true;
                    }
                    SendResult::NotConnected => {
                        debug!(to = %resource_jid, "Recipient resource not connected");
                    }
                    SendResult::ChannelFull => {
                        warn!(to = %resource_jid, "Recipient's channel full, message dropped");
                    }
                    SendResult::ChannelClosed => {
                        debug!(to = %resource_jid, "Recipient's channel closed");
                    }
                }
            }

            // Send "received" carbons to recipient's other clients
            if delivered && should_carbon {
                self.send_received_carbons_to_user(&msg_with_from, &recipient_bare, &recipient_resources).await;
            }
        }

        // Send "sent" carbons to sender's other connected clients
        if should_carbon {
            self.send_sent_carbons(&msg_with_from).await;
        }

        debug!(
            sender = %sender_jid,
            recipient = %recipient_jid,
            delivered = delivered,
            carbon_copied = should_carbon,
            "Chat message processed"
        );

        Ok(())
    }

    /// Handle a presence stanza.
    ///
    /// Routes presence based on destination:
    /// - MUC presence (to room@muc.domain/nick): Join/leave room operations
    /// - Subscription presence (subscribe/subscribed/unsubscribe/unsubscribed): RFC 6121 flow
    /// - Probe presence: Presence state query
    /// - Regular presence: Broadcast to subscribers
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

        // First check for subscription-related presence stanzas (RFC 6121)
        let sender_bare = sender_jid.to_bare();
        match parse_subscription_presence(&pres, &sender_bare)? {
            PresenceAction::Subscription(request) => {
                return self.handle_subscription_presence(request).await;
            }
            PresenceAction::Probe { from, to } => {
                return self.handle_presence_probe(from, to).await;
            }
            PresenceAction::PresenceUpdate(pres) => {
                // Continue with MUC/regular presence handling below
                // Parse the presence to see if it's a MUC action
                let muc_domain = self.room_registry.muc_domain();
                match parse_muc_presence(&pres, &sender_jid, muc_domain)? {
                    MucPresenceAction::Join(join_req) => {
                        return self.handle_muc_join(join_req).await;
                    }
                    MucPresenceAction::Leave(leave_req) => {
                        return self.handle_muc_leave(leave_req).await;
                    }
                    MucPresenceAction::NotMuc => {
                        // Regular presence update - broadcast to subscribers
                        return self.handle_presence_broadcast(&pres).await;
                    }
                }
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
        let new_occupant = room.add_occupant_with_affiliation(join_req.sender_jid.clone(), join_req.nick.clone(), Some(self.domain.as_str()));
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

    /// Handle RFC 6121 presence subscription stanzas.
    ///
    /// Routes to the appropriate handler based on subscription type:
    /// - subscribe: Request to subscribe to contact's presence
    /// - subscribed: Approval of incoming subscription request
    /// - unsubscribe: Request to unsubscribe from contact's presence
    /// - unsubscribed: Revoke incoming subscription
    #[instrument(skip(self, request), fields(sub_type = ?request.subscription_type, to = %request.to))]
    async fn handle_subscription_presence(
        &mut self,
        request: PresenceSubscriptionRequest,
    ) -> Result<(), XmppError> {
        match request.subscription_type {
            SubscriptionType::Subscribe => {
                self.handle_outbound_subscribe(request).await
            }
            SubscriptionType::Subscribed => {
                self.handle_outbound_subscribed(request).await
            }
            SubscriptionType::Unsubscribe => {
                self.handle_outbound_unsubscribe(request).await
            }
            SubscriptionType::Unsubscribed => {
                self.handle_outbound_unsubscribed(request).await
            }
        }
    }

    /// Handle outbound subscribe request (user wants to subscribe to contact).
    ///
    /// Per RFC 6121:
    /// 1. Update local roster item with ask="subscribe"
    /// 2. Route subscribe stanza to contact
    /// 3. Send roster push to user's connected resources
    #[instrument(skip(self, request), fields(contact = %request.to))]
    async fn handle_outbound_subscribe(
        &mut self,
        request: PresenceSubscriptionRequest,
    ) -> Result<(), XmppError> {
        let user_jid = request.from.clone();
        let contact_jid = request.to.clone();

        debug!(
            user = %user_jid,
            contact = %contact_jid,
            "Processing outbound subscribe request"
        );

        // Create or update roster item for the contact
        let mut item = RosterItem::new(contact_jid.clone());
        SubscriptionStateMachine::apply_outbound_subscribe(&mut item);

        // Send roster push to user's connected resources
        self.send_roster_push(&item).await;

        // Build and route the subscribe presence to the contact
        let subscribe_pres = build_subscription_presence(
            SubscriptionType::Subscribe,
            &user_jid,
            &contact_jid,
            request.status.as_deref(),
        );

        // Route to contact (local or remote)
        let stanza = Stanza::Presence(subscribe_pres);
        self.route_stanza_to_bare_jid(&contact_jid, stanza).await?;

        info!(
            user = %user_jid,
            contact = %contact_jid,
            "Sent subscribe request"
        );

        Ok(())
    }

    /// Handle outbound subscribed response (user approves contact's subscription request).
    ///
    /// Per RFC 6121:
    /// 1. Update local roster item subscription state (none→from or to→both)
    /// 2. Route subscribed stanza to contact
    /// 3. Send roster push to user's connected resources
    /// 4. Send current presence to the newly subscribed contact
    #[instrument(skip(self, request), fields(contact = %request.to))]
    async fn handle_outbound_subscribed(
        &mut self,
        request: PresenceSubscriptionRequest,
    ) -> Result<(), XmppError> {
        let user_jid = request.from.clone();
        let contact_jid = request.to.clone();

        debug!(
            user = %user_jid,
            contact = %contact_jid,
            "Processing outbound subscribed response"
        );

        // Update roster item with new subscription state
        let mut item = RosterItem::new(contact_jid.clone());
        SubscriptionStateMachine::apply_outbound_subscribed(&mut item);

        // Send roster push to user's connected resources
        self.send_roster_push(&item).await;

        // Build and route the subscribed presence to the contact
        let subscribed_pres = build_subscription_presence(
            SubscriptionType::Subscribed,
            &user_jid,
            &contact_jid,
            None,
        );

        let stanza = Stanza::Presence(subscribed_pres);
        self.route_stanza_to_bare_jid(&contact_jid, stanza).await?;

        // Send current presence to the newly subscribed contact
        // (they're now allowed to receive our presence)
        if let Some(ref jid) = self.jid {
            let available_pres = build_available_presence(
                jid,
                &contact_jid,
                None, // show
                None, // status
                0,    // priority
            );
            let stanza = Stanza::Presence(available_pres);
            self.route_stanza_to_bare_jid(&contact_jid, stanza).await?;
        }

        info!(
            user = %user_jid,
            contact = %contact_jid,
            "Approved subscription request"
        );

        Ok(())
    }

    /// Handle outbound unsubscribe request (user wants to stop receiving contact's presence).
    ///
    /// Per RFC 6121:
    /// 1. Update local roster item subscription state (to→none or both→from)
    /// 2. Route unsubscribe stanza to contact
    /// 3. Send roster push to user's connected resources
    #[instrument(skip(self, request), fields(contact = %request.to))]
    async fn handle_outbound_unsubscribe(
        &mut self,
        request: PresenceSubscriptionRequest,
    ) -> Result<(), XmppError> {
        let user_jid = request.from.clone();
        let contact_jid = request.to.clone();

        debug!(
            user = %user_jid,
            contact = %contact_jid,
            "Processing outbound unsubscribe request"
        );

        // Update roster item with new subscription state
        let mut item = RosterItem::new(contact_jid.clone());
        SubscriptionStateMachine::apply_outbound_unsubscribe(&mut item);

        // Send roster push to user's connected resources
        self.send_roster_push(&item).await;

        // Build and route the unsubscribe presence to the contact
        let unsubscribe_pres = build_subscription_presence(
            SubscriptionType::Unsubscribe,
            &user_jid,
            &contact_jid,
            None,
        );

        let stanza = Stanza::Presence(unsubscribe_pres);
        self.route_stanza_to_bare_jid(&contact_jid, stanza).await?;

        info!(
            user = %user_jid,
            contact = %contact_jid,
            "Sent unsubscribe request"
        );

        Ok(())
    }

    /// Handle outbound unsubscribed response (user revokes contact's subscription).
    ///
    /// Per RFC 6121:
    /// 1. Update local roster item subscription state (from→none or both→to)
    /// 2. Route unsubscribed stanza to contact
    /// 3. Send roster push to user's connected resources
    /// 4. Send unavailable presence to the now-unsubscribed contact
    #[instrument(skip(self, request), fields(contact = %request.to))]
    async fn handle_outbound_unsubscribed(
        &mut self,
        request: PresenceSubscriptionRequest,
    ) -> Result<(), XmppError> {
        let user_jid = request.from.clone();
        let contact_jid = request.to.clone();

        debug!(
            user = %user_jid,
            contact = %contact_jid,
            "Processing outbound unsubscribed response"
        );

        // Update roster item with new subscription state
        let mut item = RosterItem::new(contact_jid.clone());
        SubscriptionStateMachine::apply_outbound_unsubscribed(&mut item);

        // Send roster push to user's connected resources
        self.send_roster_push(&item).await;

        // Build and route the unsubscribed presence to the contact
        let unsubscribed_pres = build_subscription_presence(
            SubscriptionType::Unsubscribed,
            &user_jid,
            &contact_jid,
            None,
        );

        let stanza = Stanza::Presence(unsubscribed_pres);
        self.route_stanza_to_bare_jid(&contact_jid, stanza).await?;

        // Send unavailable presence to the contact (they can no longer see us)
        let unavailable_pres = build_unavailable_presence(
            &user_jid,
            &contact_jid,
        );
        let stanza = Stanza::Presence(unavailable_pres);
        self.route_stanza_to_bare_jid(&contact_jid, stanza).await?;

        info!(
            user = %user_jid,
            contact = %contact_jid,
            "Revoked subscription"
        );

        Ok(())
    }

    /// Handle a presence probe request.
    ///
    /// Per RFC 6121, a presence probe is sent by a server to request the
    /// current presence of a user. This is typically used when:
    /// - A user comes online and needs to know contacts' presence
    /// - A server needs to verify a user's presence state
    #[instrument(skip(self), fields(from = %from, to = %to))]
    async fn handle_presence_probe(
        &mut self,
        from: jid::BareJid,
        to: jid::BareJid,
    ) -> Result<(), XmppError> {
        debug!(
            from = %from,
            to = %to,
            "Processing presence probe"
        );

        // Check if the requesting user has a subscription that allows them
        // to receive the target's presence (subscription=to or both)
        // For now, we'll respond with current presence if the target is connected

        // Get all connected resources for the target user
        let resources = self.connection_registry.get_resources_for_user(&to);

        if resources.is_empty() {
            // User is offline - send unavailable presence
            let unavailable = build_unavailable_presence(&to, &from);
            let stanza = Stanza::Presence(unavailable);
            self.route_stanza_to_bare_jid(&from, stanza).await?;

            debug!(
                from = %from,
                to = %to,
                "Sent unavailable presence in response to probe (user offline)"
            );
        } else {
            // User is online - send available presence from each resource
            for resource_jid in resources {
                let available = build_available_presence(
                    &resource_jid,
                    &from,
                    None, // TODO: Get actual show state
                    None, // TODO: Get actual status
                    0,    // TODO: Get actual priority
                );
                let stanza = Stanza::Presence(available);
                self.route_stanza_to_bare_jid(&from, stanza).await?;
            }

            debug!(
                from = %from,
                to = %to,
                "Sent available presence in response to probe"
            );
        }

        Ok(())
    }

    /// Handle presence broadcast to subscribers.
    ///
    /// When a user sends a presence update (available/unavailable/show change),
    /// broadcast it to all contacts with subscription=from or subscription=both.
    #[instrument(skip(self, pres), fields(presence_type = ?pres.type_))]
    async fn handle_presence_broadcast(
        &mut self,
        pres: &xmpp_parsers::presence::Presence,
    ) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => return Ok(()), // Not bound yet
        };

        let sender_bare = sender_jid.to_bare();

        debug!(
            sender = %sender_jid,
            presence_type = ?pres.type_,
            "Processing presence broadcast"
        );

        // For now, we don't have roster storage integration, so we can't
        // determine which contacts should receive this presence.
        // This is a stub that logs the presence but doesn't broadcast.
        //
        // TODO: When roster storage is integrated:
        // 1. Get user's roster
        // 2. For each contact with subscription=from or subscription=both
        // 3. Send presence to that contact

        // Log initial presence for now
        if matches!(pres.type_, xmpp_parsers::presence::Type::None) {
            // Available presence (no type = available)
            info!(
                sender = %sender_bare,
                "User sent initial available presence"
            );
        } else if matches!(pres.type_, xmpp_parsers::presence::Type::Unavailable) {
            info!(
                sender = %sender_bare,
                "User sent unavailable presence"
            );
        }

        Ok(())
    }

    /// Route a stanza to a bare JID (all connected resources or offline storage).
    ///
    /// Helper method to route presence and other stanzas to users.
    async fn route_stanza_to_bare_jid(
        &self,
        target: &jid::BareJid,
        stanza: Stanza,
    ) -> Result<(), XmppError> {
        // Get all connected resources for the target
        let resources = self.connection_registry.get_resources_for_user(target);

        if resources.is_empty() {
            // User is offline - presence stanzas are typically not stored
            // but subscription stanzas should be queued
            debug!(
                target = %target,
                "Target user offline, stanza not delivered"
            );
            return Ok(());
        }

        // Send to all connected resources
        for resource_jid in resources {
            let _ = self.connection_registry.send_to(&resource_jid, stanza.clone()).await;
        }

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

        // Check if this is a carbons enable request
        if is_carbons_enable(&iq) {
            return self.handle_carbons_enable(iq).await;
        }

        // Check if this is a carbons disable request
        if is_carbons_disable(&iq) {
            return self.handle_carbons_disable(iq).await;
        }

        // Check if this is an ISR token refresh request (XEP-0397)
        if is_isr_token_request(&iq) {
            return self.handle_isr_token_request(iq).await;
        }

        // Check if this is a roster get request (RFC 6121)
        if is_roster_get(&iq) {
            return self.handle_roster_get(iq).await;
        }

        // Check if this is a roster set request (RFC 6121)
        if is_roster_set(&iq) {
            return self.handle_roster_set(iq).await;
        }

        // Check if this is a vCard get request (XEP-0054)
        if is_vcard_get(&iq) {
            return self.handle_vcard_get(iq).await;
        }

        // Check if this is a vCard set request (XEP-0054)
        if is_vcard_set(&iq) {
            return self.handle_vcard_set(iq).await;
        }

        // Check if this is an HTTP File Upload slot request (XEP-0363)
        if is_upload_request(&iq) {
            return self.handle_upload_slot_request(iq).await;
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

    /// Handle XEP-0280 carbons enable request.
    ///
    /// Enables message carbons for this connection. When enabled, messages
    /// sent or received by the user on any device are copied to all other
    /// connected devices.
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_carbons_enable(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        debug!("Enabling message carbons for connection");

        self.carbons_enabled = true;

        // Send success response
        let response = build_carbons_result(&iq);
        self.stream.write_stanza(&Stanza::Iq(response)).await?;

        info!("Message carbons enabled");
        Ok(())
    }

    /// Handle XEP-0280 carbons disable request.
    ///
    /// Disables message carbons for this connection.
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_carbons_disable(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        debug!("Disabling message carbons for connection");

        self.carbons_enabled = false;

        // Send success response
        let response = build_carbons_result(&iq);
        self.stream.write_stanza(&Stanza::Iq(response)).await?;

        info!("Message carbons disabled");
        Ok(())
    }

    /// Handle XEP-0397 ISR token refresh request.
    ///
    /// Clients can request a new ISR token during an active session using:
    /// ```xml
    /// <iq type='get' id='...'>
    ///   <token-request xmlns='urn:xmpp:isr:0'/>
    /// </iq>
    /// ```
    ///
    /// The server responds with a new token, invalidating the old one:
    /// ```xml
    /// <iq type='result' id='...'>
    ///   <token xmlns='urn:xmpp:isr:0' expiry='ISO8601'>NEW_TOKEN</token>
    /// </iq>
    /// ```
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_isr_token_request(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        debug!("Received ISR token refresh request");

        // Require an established session
        let session = match &self.session {
            Some(s) => s,
            None => {
                warn!("ISR token refresh requested before session established");
                let error = build_isr_token_error(&iq, "not-authorized");
                self.stream.write_stanza(&Stanza::Iq(error)).await?;
                return Ok(());
            }
        };

        // Require a JID to be bound
        let jid = match &self.jid {
            Some(j) => j.to_bare(),
            None => {
                warn!("ISR token refresh requested before JID bound");
                let error = build_isr_token_error(&iq, "not-authorized");
                self.stream.write_stanza(&Stanza::Iq(error)).await?;
                return Ok(());
            }
        };

        // If there's a current token, invalidate it
        if let Some(ref old_token) = self.current_isr_token {
            self.isr_token_store.consume_token(old_token);
            debug!(
                old_token = %&old_token[..old_token.len().min(8)],
                "Invalidated old ISR token"
            );
        }

        // Create a new ISR token
        let new_token = if self.sm_state.enabled {
            // Include SM state if Stream Management is enabled
            let sm_stream_id = self.sm_state.stream_id.clone().unwrap_or_default();
            self.isr_token_store.create_token_with_sm(
                session.did.clone(),
                jid.clone(),
                sm_stream_id,
                self.sm_state.inbound_count,
                self.sm_state.outbound_count,
            )
        } else {
            self.isr_token_store.create_token(session.did.clone(), jid.clone())
        };

        // Store the new token ID
        self.current_isr_token = Some(new_token.token.clone());

        // Build and send the response
        let response = build_isr_token_result(&iq, &new_token);
        self.stream.write_stanza(&Stanza::Iq(response)).await?;

        info!(
            jid = %jid,
            token_expiry = %new_token.expiry,
            sm_enabled = self.sm_state.enabled,
            "ISR token refreshed"
        );

        Ok(())
    }

    /// Send carbon copies of a sent message to other connected resources.
    ///
    /// When the user sends a message from this client, we forward it as a
    /// "sent" carbon to all other connected clients that have carbons enabled.
    async fn send_sent_carbons(&self, msg: &xmpp_parsers::message::Message) {
        let sender_jid = match &self.jid {
            Some(jid) => jid,
            None => return,
        };

        // Get the bare JID to find all resources
        let bare_jid = sender_jid.to_bare();

        // Get all other resources for this user with carbons enabled
        let other_resources = self
            .connection_registry
            .get_other_resources_for_user(&bare_jid, sender_jid);

        if other_resources.is_empty() {
            return;
        }

        debug!(
            resource_count = other_resources.len(),
            "Sending sent carbons to other resources"
        );

        for resource_jid in other_resources {
            let carbon = build_sent_carbon(
                msg,
                &bare_jid.to_string(),
                &resource_jid.to_string(),
            );

            let stanza = Stanza::Message(carbon);
            let _ = self.connection_registry.send_to(&resource_jid, stanza).await;
        }
    }

    /// Send carbon copies of a received message to other connected resources.
    ///
    /// When the user receives a message, we forward it as a "received" carbon
    /// to all other connected clients that have carbons enabled.
    #[allow(dead_code)] // Used for self-targeting carbons, currently we use send_received_carbons_to_user instead
    async fn send_received_carbons(&self, msg: &xmpp_parsers::message::Message) {
        let recipient_jid = match &self.jid {
            Some(jid) => jid,
            None => return,
        };

        // Get the bare JID to find all resources
        let bare_jid = recipient_jid.to_bare();

        // Get all other resources for this user
        let other_resources = self
            .connection_registry
            .get_other_resources_for_user(&bare_jid, recipient_jid);

        if other_resources.is_empty() {
            return;
        }

        debug!(
            resource_count = other_resources.len(),
            "Sending received carbons to other resources"
        );

        for resource_jid in other_resources {
            let carbon = build_received_carbon(
                msg,
                &bare_jid.to_string(),
                &resource_jid.to_string(),
            );

            let stanza = Stanza::Message(carbon);
            let _ = self.connection_registry.send_to(&resource_jid, stanza).await;
        }
    }

    /// Send "received" carbon copies to a specific user's other connected resources.
    ///
    /// This is used when routing a message to a recipient - we need to send
    /// received carbons to the recipient's other devices, not the sender's.
    ///
    /// The `delivered_resources` parameter contains the resources that received
    /// the original message, so we exclude those from carbon delivery.
    async fn send_received_carbons_to_user(
        &self,
        msg: &xmpp_parsers::message::Message,
        recipient_bare: &jid::BareJid,
        delivered_resources: &[FullJid],
    ) {
        // Get all resources for the recipient user
        let all_resources = self.connection_registry.get_resources_for_user(recipient_bare);

        // Filter to resources that didn't receive the original (other devices)
        // In practice, if the message was routed to all resources, we might not need carbons
        // But for now, we send received carbons to all resources except the primary recipient
        // Note: XEP-0280 says received carbons go to OTHER resources, so if we delivered
        // to all resources, there are no "other" resources to carbon-copy to.
        // However, the spec intention is that resources with carbons enabled get the carbon,
        // while all resources get the original message.

        // For simplicity, we skip sending received carbons when we've already
        // delivered to all connected resources, since they all have the message.
        // The main use case for received carbons is when a specific resource is targeted.
        if delivered_resources.len() >= all_resources.len() {
            debug!("All recipient resources received the message, skipping received carbons");
            return;
        }

        // Find resources that didn't get the original message
        let other_resources: Vec<&FullJid> = all_resources
            .iter()
            .filter(|r| !delivered_resources.contains(r))
            .collect();

        if other_resources.is_empty() {
            return;
        }

        debug!(
            resource_count = other_resources.len(),
            recipient = %recipient_bare,
            "Sending received carbons to recipient's other resources"
        );

        for resource_jid in other_resources {
            let carbon = build_received_carbon(
                msg,
                &recipient_bare.to_string(),
                &resource_jid.to_string(),
            );

            let stanza = Stanza::Message(carbon);
            let _ = self.connection_registry.send_to(resource_jid, stanza).await;
        }
    }

    /// Handle RFC 6121 roster get request.
    ///
    /// Returns the user's roster with all contact items:
    /// ```xml
    /// <iq type='result' id='...'>
    ///   <query xmlns='jabber:iq:roster'>
    ///     <item jid='...' name='...' subscription='...'/>
    ///   </query>
    /// </iq>
    /// ```
    ///
    /// Note: This is a stub implementation that returns an empty roster.
    /// Full roster storage integration requires AppState to implement
    /// roster storage methods.
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_roster_get(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => {
                warn!("Roster get received before JID bound");
                return Err(XmppError::not_authorized(Some(
                    "Session not established".to_string(),
                )));
            }
        };

        debug!(jid = %sender_jid, "Processing roster get request");

        // Parse the roster query (mainly for version tracking)
        let query = parse_roster_get(&iq)?;

        // TODO: Integrate with AppState for actual roster storage
        // For now, return an empty roster as a stub implementation.
        // The roster items would come from the application's storage layer.
        let items: Vec<RosterItem> = Vec::new();

        // Build and send the roster result
        let response = build_roster_result(&iq, &items, query.ver.as_deref());
        self.stream.write_stanza(&Stanza::Iq(response)).await?;

        debug!(
            item_count = items.len(),
            ver = ?query.ver,
            "Sent roster get response"
        );

        Ok(())
    }

    /// Handle RFC 6121 roster set request.
    ///
    /// Processes add, update, or remove operations on roster items.
    /// After processing, sends roster push to all connected resources.
    ///
    /// Note: This is a stub implementation that acknowledges the request
    /// but does not persist changes. Full roster storage integration
    /// requires AppState to implement roster storage methods.
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_roster_set(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => {
                warn!("Roster set received before JID bound");
                return Err(XmppError::not_authorized(Some(
                    "Session not established".to_string(),
                )));
            }
        };

        debug!(jid = %sender_jid, "Processing roster set request");

        // Parse the roster set query
        let query = parse_roster_set(&iq)?;

        // Per RFC 6121, roster set should have exactly one item
        let item = query.items.first().ok_or_else(|| {
            XmppError::bad_request(Some("Roster set must contain an item".to_string()))
        })?;

        debug!(
            contact_jid = %item.jid,
            subscription = %item.subscription,
            is_remove = item.subscription.is_remove(),
            "Processing roster item"
        );

        // TODO: Integrate with AppState for actual roster storage
        // For now, acknowledge the request without persisting changes.

        // Send empty result to acknowledge the roster set
        let response = build_roster_result_empty(&iq);
        self.stream.write_stanza(&Stanza::Iq(response)).await?;

        // RFC 6121: Send roster push to all connected resources
        // This notifies all user's clients about the roster change
        self.send_roster_push(item).await;

        info!(
            contact = %item.jid,
            subscription = %item.subscription,
            "Roster set processed"
        );

        Ok(())
    }

    /// Send roster push to all connected resources for the user.
    ///
    /// Per RFC 6121 Section 2.1.6, after a roster item is modified,
    /// the server must send a roster push to all of the user's
    /// connected resources that have requested the roster.
    async fn send_roster_push(&self, item: &RosterItem) {
        let sender_jid = match &self.jid {
            Some(jid) => jid,
            None => return,
        };

        let bare_jid = sender_jid.to_bare();

        // Get all connected resources for this user (including self)
        let resources = self
            .connection_registry
            .get_resources_for_user(&bare_jid);

        if resources.is_empty() {
            return;
        }

        debug!(
            resource_count = resources.len(),
            contact = %item.jid,
            "Sending roster push to connected resources"
        );

        for resource_jid in resources {
            // Generate a unique push ID
            let push_id = format!("push-{}", uuid::Uuid::new_v4());

            let push = build_roster_push(
                &push_id,
                &resource_jid.to_string(),
                item,
                None, // No versioning for now
            );

            let stanza = Stanza::Iq(push);
            let result = self.connection_registry.send_to(&resource_jid, stanza).await;

            match result {
                SendResult::Sent => {
                    debug!(to = %resource_jid, "Roster push sent");
                }
                SendResult::NotConnected => {
                    debug!(to = %resource_jid, "Resource not connected for roster push");
                }
                SendResult::ChannelFull | SendResult::ChannelClosed => {
                    warn!(to = %resource_jid, "Failed to send roster push");
                }
            }
        }
    }

    /// Handle XEP-0054 vCard get request.
    ///
    /// Retrieves a user's vCard. If no 'to' attribute is specified or it matches
    /// the sender's bare JID, returns the sender's own vCard. Otherwise returns
    /// the vCard of the specified user (if they exist).
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_vcard_get(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => {
                warn!("vCard get received before JID bound");
                return Err(XmppError::not_authorized(Some(
                    "Session not established".to_string(),
                )));
            }
        };

        // Determine whose vCard to retrieve
        let target_jid: jid::BareJid = match &iq.to {
            Some(jid) => jid.to_bare(),
            None => sender_jid.to_bare(),
        };

        debug!(
            from = %sender_jid,
            target = %target_jid,
            "Processing vCard get request"
        );

        // Retrieve the vCard from storage
        let vcard_xml = self.app_state.get_vcard(&target_jid).await?;

        let response = match vcard_xml {
            Some(xml) => {
                // Parse the stored XML to build the response
                match xml.parse::<minidom::Element>() {
                    Ok(elem) => {
                        let vcard = crate::xep::xep0054::parse_vcard_element(&elem)
                            .unwrap_or_default();
                        build_vcard_response(&iq, &vcard)
                    }
                    Err(_) => {
                        // If stored XML is invalid, return empty vCard
                        warn!(target = %target_jid, "Stored vCard XML invalid, returning empty");
                        build_empty_vcard_response(&iq)
                    }
                }
            }
            None => {
                // No vCard stored, return empty vCard
                build_empty_vcard_response(&iq)
            }
        };

        self.stream.write_stanza(&Stanza::Iq(response)).await?;

        debug!(target = %target_jid, "vCard get response sent");
        Ok(())
    }

    /// Handle XEP-0054 vCard set request.
    ///
    /// Updates the authenticated user's own vCard. Users can only update
    /// their own vCard, not those of other users.
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_vcard_set(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => {
                warn!("vCard set received before JID bound");
                return Err(XmppError::not_authorized(Some(
                    "Session not established".to_string(),
                )));
            }
        };

        // Users can only update their own vCard
        if let Some(ref to) = iq.to {
            let target_bare = to.to_bare();
            if target_bare != sender_jid.to_bare() {
                warn!(
                    from = %sender_jid,
                    to = %target_bare,
                    "Attempt to update another user's vCard"
                );
                return Err(XmppError::forbidden(Some(
                    "Cannot update another user's vCard".to_string(),
                )));
            }
        }

        debug!(jid = %sender_jid, "Processing vCard set request");

        // Parse the vCard from the IQ
        let vcard = parse_vcard_from_iq(&iq).map_err(|e| {
            XmppError::bad_request(Some(format!("Invalid vCard: {}", e)))
        })?;

        // Build the vCard XML for storage
        let vcard_elem = crate::xep::xep0054::build_vcard_element(&vcard);
        let vcard_xml = String::from(&vcard_elem);

        // Store the vCard
        self.app_state
            .set_vcard(&sender_jid.to_bare(), &vcard_xml)
            .await?;

        // Send success response
        let response = build_vcard_success(&iq);
        self.stream.write_stanza(&Stanza::Iq(response)).await?;

        info!(
            jid = %sender_jid,
            full_name = ?vcard.full_name,
            "vCard updated"
        );

        Ok(())
    }

    /// Handle XEP-0363 HTTP File Upload slot request.
    ///
    /// Processes upload slot requests from authenticated users. Validates the
    /// request, checks file size limits, and returns PUT/GET URLs for the file.
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_upload_slot_request(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => {
                warn!("Upload slot request received before JID bound");
                return Err(XmppError::not_authorized(Some(
                    "Session not established".to_string(),
                )));
            }
        };

        debug!(
            from = %sender_jid,
            "Processing HTTP upload slot request"
        );

        // Check if uploads are enabled
        if !self.app_state.upload_enabled() {
            let error_response = build_upload_error(&iq.id, &UploadError::NotAllowed);
            self.stream.write_raw(&error_response).await?;
            return Ok(());
        }

        // Parse the upload request
        let request = match parse_upload_request(&iq) {
            Ok(req) => req,
            Err(e) => {
                let error_response = build_upload_error(&iq.id, &e);
                self.stream.write_raw(&error_response).await?;
                return Ok(());
            }
        };

        // Check file size limits
        let max_size = self.app_state.max_upload_size();
        if request.size > max_size {
            let error_response = build_upload_error(
                &iq.id,
                &UploadError::FileTooLarge { max_size },
            );
            self.stream.write_raw(&error_response).await?;
            return Ok(());
        }

        // Create the upload slot
        let slot_info = match self
            .app_state
            .create_upload_slot(
                &sender_jid.to_bare(),
                &request.filename,
                request.size,
                request.content_type.as_deref(),
            )
            .await
        {
            Ok(info) => info,
            Err(e) => {
                warn!(error = %e, "Failed to create upload slot");
                let error_response = build_upload_error(
                    &iq.id,
                    &UploadError::InternalError(e.to_string()),
                );
                self.stream.write_raw(&error_response).await?;
                return Ok(());
            }
        };

        // Build the success response
        let slot = UploadSlot {
            put_url: slot_info.put_url,
            put_headers: slot_info.put_headers,
            get_url: slot_info.get_url,
        };
        let response = build_upload_slot_response(&iq, &slot);
        self.stream.write_stanza(&Stanza::Iq(response)).await?;

        debug!(
            from = %sender_jid,
            filename = %request.filename,
            size = request.size,
            "Upload slot created"
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
