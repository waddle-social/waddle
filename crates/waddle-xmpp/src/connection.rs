//! Connection actor for handling individual XMPP client connections.

use std::net::SocketAddr;
use std::sync::Arc;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;
use chrono::Utc;
use jid::FullJid;
use tokio::net::TcpStream;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, instrument, warn};
use xmpp_parsers::iq::IqType;
use xmpp_parsers::message::MessageType;
use xmpp_parsers::presence::Type as PresenceType;

use crate::carbons::{
    build_carbons_result, build_received_carbon, build_sent_carbon, is_carbons_disable,
    is_carbons_enable, should_copy_message,
};
use crate::disco::{
    build_disco_info_response_with_extensions, build_disco_items_response,
    build_server_info_abuse_form, is_disco_info_query, is_disco_items_query, muc_room_features,
    muc_service_features, parse_disco_info_query, parse_disco_items_query, pubsub_service_features,
    server_features, upload_service_features, DiscoItem, Identity,
};
use crate::isr::{
    build_isr_token_error, build_isr_token_result, is_isr_token_request, SharedIsrTokenStore,
};
use crate::mam::{
    add_stanza_id, build_fin_iq, build_result_messages, is_mam_query, parse_mam_query,
    ArchivedMessage, MamStorage,
};
use crate::metrics::{record_muc_occupant_count, record_muc_presence};
use crate::muc::{
    admin::{
        build_admin_result, build_admin_set_result, build_role_result, is_muc_admin_iq,
        is_role_change_query, parse_admin_query,
    },
    affiliation::{AffiliationResolver, AppStateAffiliationResolver},
    build_affiliation_change_presence, build_ban_presence, build_kick_presence,
    build_leave_presence, build_occupant_presence, build_role_change_presence, is_muc_owner_get,
    is_muc_owner_set,
    owner::{
        apply_config_form, build_config_form, build_config_result, build_destroy_notification,
        build_owner_set_result, parse_owner_query, OwnerAction,
    },
    parse_muc_presence, MucJoinRequest, MucLeaveRequest, MucMessage, MucPresenceAction,
    MucRoomRegistry,
};
use crate::parser::ParsedStanza;
use crate::presence::{
    build_available_presence, build_subscription_presence, build_unavailable_presence,
    parse_subscription_presence, PresenceAction, PresenceSubscriptionRequest,
    SubscriptionStateMachine, SubscriptionType,
};
use crate::pubsub::{
    build_pubsub_error, build_pubsub_items_result, build_pubsub_publish_result,
    build_pubsub_success, is_pubsub_iq, parse_pubsub_iq, PubSubError, PubSubItem, PubSubRequest,
    PubSubStorage,
};
use crate::registry::{ConnectionRegistry, OutboundStanza, SendResult};
use crate::roster::{
    build_roster_push, build_roster_result, build_roster_result_empty, is_roster_get,
    is_roster_set, parse_roster_get, parse_roster_set, RosterItem, Subscription,
};
use crate::routing::StanzaRouter;
use crate::stream::{PreAuthResult, SaslAuthResult, XmppStream};
use crate::stream_management::{SmSessionRegistry, StreamManagementState};
use crate::types::ConnectionState;
use crate::types::{Affiliation, Role};
use crate::xep::xep0049::{
    build_private_storage_result, build_private_storage_success, is_private_storage_query,
    parse_private_storage_get, parse_private_storage_set,
};
use crate::xep::xep0054::{
    build_empty_vcard_response, build_vcard_response, build_vcard_success, is_vcard_get,
    is_vcard_set, parse_vcard_from_iq,
};
use crate::xep::xep0077::RegistrationError;
use crate::xep::xep0191::{
    build_block_push, build_blocking_error, build_blocking_success, build_blocklist_response,
    build_unblock_push, is_blocking_query, parse_blocking_request, BlockingRequest,
};
use crate::xep::xep0199::{build_ping_result, is_ping};
use crate::xep::xep0249::{parse_direct_invite_from_message, DirectInvite};
use crate::xep::xep0363::{
    build_upload_error, build_upload_slot_response, is_upload_request, parse_upload_request,
    UploadError, UploadSlot,
};
use crate::xep::xep0398::AvatarConversion;
use crate::{AppState, Session, XmppError};

/// Size of the outbound message channel buffer.
const OUTBOUND_CHANNEL_SIZE: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BareMessageDelivery {
    /// Deliver to all available resources with non-negative priority.
    AllNonNegative,
    /// Deliver to one highest-priority resource or all when priorities tie.
    HighestOrAll,
}

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
    /// XEP-0198 Stream Management session registry (for resumption)
    sm_session_registry: Arc<dyn SmSessionRegistry>,
    /// XEP-0280 Message Carbons enabled state
    carbons_enabled: bool,
    /// XEP-0397 ISR token store for instant stream resumption
    isr_token_store: SharedIsrTokenStore,
    /// Current ISR token for this connection (if any)
    current_isr_token: Option<String>,
    /// Optional stanza router for S2S federation routing
    /// When present and federation is enabled, allows routing MUC messages
    /// to remote occupants via S2S connections.
    stanza_router: Option<Arc<StanzaRouter>>,
    /// XEP-0352 Client State Indication: current client state
    client_state: crate::xep::xep0352::ClientState,
    /// XEP-0352 Client State Indication: stanza buffer for inactive clients
    /// When the client is inactive, non-critical stanzas are buffered here
    /// and flushed when the client becomes active again.
    csi_buffer: Vec<Stanza>,
    /// XEP-0060/XEP-0163 PubSub/PEP storage for bookmarks and other PEP data
    pubsub_storage: Arc<dyn PubSubStorage + Send + Sync>,
    /// XEP-0153 vCard avatar hash (SHA-1 hex) for inclusion in presence stanzas
    avatar_hash: Option<String>,
    /// Most recent available presence stanza sent by this resource.
    /// Used to replay "current presence" after subscription approval (RFC 6121).
    last_available_presence: Option<xmpp_parsers::presence::Presence>,
    /// XEP-0398 guard flag to prevent infinite avatar conversion loops
    converting_avatar: bool,
}

impl<S: AppState, M: MamStorage> ConnectionActor<S, M> {
    /// Handle a new incoming connection.
    #[instrument(
        name = "xmpp.connection.handle",
        skip(tcp_stream, tls_acceptor, app_state, room_registry, connection_registry, mam_storage, isr_token_store, sm_session_registry, pubsub_storage),
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
        sm_session_registry: Arc<dyn SmSessionRegistry>,
        registration_enabled: bool,
        pubsub_storage: Arc<dyn PubSubStorage + Send + Sync>,
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
            sm_session_registry,
            carbons_enabled: false,
            isr_token_store,
            current_isr_token: None,
            stanza_router: None, // Federation routing disabled by default; can be set via set_stanza_router()
            client_state: crate::xep::xep0352::ClientState::default(), // XEP-0352: starts as Active
            csi_buffer: Vec::new(), // XEP-0352: stanza buffer for inactive clients
            pubsub_storage,      // XEP-0060/0163 PubSub/PEP storage (shared across connections)
            avatar_hash: None,   // XEP-0153: computed on bind from stored vCard
            last_available_presence: None,
            converting_avatar: false, // XEP-0398: guard against infinite conversion loops
        };

        actor.run(tls_acceptor, registration_enabled).await
    }

    /// Main connection loop.
    async fn run(
        &mut self,
        tls_acceptor: TlsAcceptor,
        registration_enabled: bool,
    ) -> Result<(), XmppError> {
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
        self.stream
            .send_features_starttls_with_registration(registration_enabled)
            .await?;

        // Wait for STARTTLS
        self.state = ConnectionState::StartTls;
        self.stream.handle_starttls(tls_acceptor).await?;

        // TLS established, send new features (SASL with optional registration)
        self.state = ConnectionState::TlsEstablished;
        let _header = self.stream.read_stream_header().await?;

        // Enable XEP-0077 In-Band Registration in stream features if configured
        self.stream
            .send_features_sasl_with_registration(registration_enabled)
            .await?;

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
        self.connection_registry
            .register(full_jid.clone(), outbound_tx);
        self.outbound_rx = Some(outbound_rx);

        info!(jid = %full_jid, "Session established and registered");

        // XEP-0153: Pre-compute avatar hash from stored vCard
        if let Ok(Some(vcard_xml)) = self.app_state.get_vcard(&full_jid.to_bare()).await {
            if let Ok(elem) = vcard_xml.parse::<minidom::Element>() {
                if let Ok(vcard) = crate::xep::xep0054::parse_vcard_element(&elem) {
                    if let Some(ref photo) = vcard.photo {
                        self.avatar_hash =
                            crate::xep::xep0153::compute_photo_hash_from_base64(&photo.data);
                        debug!(
                            jid = %full_jid,
                            avatar_hash = ?self.avatar_hash,
                            "Pre-computed avatar hash from stored vCard"
                        );
                    }
                }
            }
        }

        // Main stanza processing loop
        let result = self.process_stanzas().await;

        // Store SM session for potential resumption (if enabled and resumable)
        if let Some(ref jid) = self.jid {
            if let Some(detached) = self.sm_state.to_detached_session(jid.clone()) {
                debug!(
                    stream_id = %detached.stream_id,
                    jid = %jid,
                    unacked_count = detached.unacked_stanzas.len(),
                    "Storing detached SM session for potential resumption"
                );
                if let Err(e) = self.sm_session_registry.store_session(detached).await {
                    warn!(error = %e, "Failed to store SM session for resumption");
                }
            }
        }

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
                    let jid = self.normalize_plain_jid(&jid);
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
                            if let Ok(Some(session)) =
                                self.try_native_plain_fallback(&jid, &token).await
                            {
                                self.send_sasl_success_with_isr(&session, &jid).await?;
                                return Ok((jid, session));
                            }

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
                    self.stream
                        .send_oauthbearer_discovery(&discovery_url)
                        .await?;

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
                            self.stream
                                .send_scram_challenge(&server_first_message_b64)
                                .await?;

                            // Continue the SCRAM exchange
                            match self
                                .stream
                                .continue_scram_auth(
                                    scram_server,
                                    &creds.stored_key,
                                    &creds.server_key,
                                )
                                .await
                            {
                                Ok(SaslAuthResult::ScramSha256Complete { username }) => {
                                    // Authentication successful - create session for native user
                                    // The JID is username@domain
                                    let jid: jid::BareJid = format!("{}@{}", username, self.domain)
                                        .parse()
                                        .map_err(|e| {
                                            XmppError::auth_failed(format!("Invalid JID: {}", e))
                                        })?;

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
                                    return Err(XmppError::auth_failed(
                                        "SCRAM authentication failed",
                                    ));
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
                                .send_registration_form(
                                    &id,
                                    Some("Choose a username and password to register."),
                                )
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
                let jid = self.normalize_plain_jid(&jid);
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
                        if let Ok(Some(session)) =
                            self.try_native_plain_fallback(&jid, &token).await
                        {
                            self.send_sasl_success_with_isr(&session, &jid).await?;
                            return Ok((jid, session));
                        }

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
                self.stream
                    .send_oauthbearer_discovery(&discovery_url)
                    .await?;
                debug!(discovery_url = %discovery_url, "Sent OAUTHBEARER discovery");
                // Need to wait for next auth attempt - this would need to loop
                // For now, return an error indicating client should reconnect
                Err(XmppError::auth_failed(
                    "OAuth discovery sent - complete OAuth flow and reconnect",
                ))
            }
            SaslAuthResult::ScramSha256Challenge {
                username,
                server_first_message_b64,
                scram_server,
            } => {
                // SCRAM-SHA-256: Look up credentials and continue
                match self.app_state.lookup_scram_credentials(&username).await {
                    Ok(Some(creds)) => {
                        self.stream
                            .send_scram_challenge(&server_first_message_b64)
                            .await?;
                        match self
                            .stream
                            .continue_scram_auth(scram_server, &creds.stored_key, &creds.server_key)
                            .await
                        {
                            Ok(SaslAuthResult::ScramSha256Complete { username }) => {
                                let jid: jid::BareJid =
                                    format!("{}@{}", username, self.domain).parse().map_err(
                                        |e| XmppError::auth_failed(format!("Invalid JID: {}", e)),
                                    )?;
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

    fn normalize_plain_jid(&self, jid: &jid::BareJid) -> jid::BareJid {
        if jid.node().is_some() {
            return jid.clone();
        }

        format!("{}@{}", jid.domain(), self.domain)
            .parse()
            .unwrap_or_else(|_| jid.clone())
    }

    async fn try_native_plain_fallback(
        &self,
        jid: &jid::BareJid,
        token: &str,
    ) -> Result<Option<Session>, XmppError> {
        // Some clients provide only "username" in SASL PLAIN authcid. In that case
        // the parser treats it as a domain-only BareJid, so derive the username from
        // the domain part.
        let username = jid
            .node()
            .map(|n| n.to_string())
            .unwrap_or_else(|| jid.domain().to_string());

        let creds = match self.app_state.lookup_scram_credentials(&username).await {
            Ok(Some(creds)) => creds,
            Ok(None) => return Ok(None),
            Err(err) => {
                warn!(
                    error = %err,
                    username = %username,
                    "Failed to lookup SCRAM credentials during PLAIN fallback"
                );
                return Ok(None);
            }
        };

        let salt = match BASE64_STANDARD.decode(creds.salt_b64) {
            Ok(salt) => salt,
            Err(_) => return Ok(None),
        };
        let (stored_key, server_key) =
            crate::auth::scram::generate_scram_keys(token, &salt, creds.iterations);
        if stored_key != creds.stored_key || server_key != creds.server_key {
            return Ok(None);
        }

        Ok(Some(Session {
            did: jid.to_string(),
            jid: jid.clone(),
            created_at: Utc::now(),
            expires_at: Utc::now() + chrono::Duration::hours(24),
        }))
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
                self.stream
                    .send_registration_success(id)
                    .await
                    .map_err(|e| {
                        RegistrationError::InternalError(format!("Failed to send success: {}", e))
                    })?;

                Ok(())
            }
            Err(e) => {
                // Map XmppError to RegistrationError
                if e.to_string().contains("already exists") || e.to_string().contains("conflict") {
                    Err(RegistrationError::Conflict)
                } else if e.to_string().contains("not acceptable")
                    || e.to_string().contains("invalid")
                {
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
        if !isr_token_in_sasl_success_enabled() {
            self.current_isr_token = None;
            self.stream.send_sasl_success().await?;
            debug!(
                did = %session.did,
                jid = %jid,
                "Sent SASL success without ISR token (compat mode)"
            );
            return Ok(());
        }

        // Create an ISR token for this session
        let isr_token = self
            .isr_token_store
            .create_token(session.did.clone(), jid.clone());

        // Store the token ID for this connection
        self.current_isr_token = Some(isr_token.token.clone());

        // Send success with ISR token
        self.stream
            .send_sasl_success_with_isr(&isr_token.to_xml())
            .await?;

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
                            // Use CSI-aware sending to buffer non-critical stanzas when client is inactive
                            if let Err(e) = self.send_stanza_with_csi(outbound_stanza.stanza).await {
                                warn!(error = %e, "Error writing outbound stanza");
                                // Don't break - the client might still be readable
                            }
                            // Track outbound stanzas for SM acknowledgment
                            // Note: buffered stanzas are also counted since they'll be sent eventually
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
                let msg = element
                    .try_into()
                    .map_err(|e| XmppError::xml_parse(format!("Invalid message: {:?}", e)))?;
                // Increment inbound count for SM
                if self.sm_state.enabled {
                    self.sm_state.increment_inbound();
                }
                self.handle_stanza(Stanza::Message(msg)).await
            }
            ParsedStanza::Presence(element) => {
                let pres = element
                    .try_into()
                    .map_err(|e| XmppError::xml_parse(format!("Invalid presence: {:?}", e)))?;
                // Increment inbound count for SM
                if self.sm_state.enabled {
                    self.sm_state.increment_inbound();
                }
                self.handle_stanza(Stanza::Presence(pres)).await
            }
            ParsedStanza::Iq(element) => {
                let iq: xmpp_parsers::iq::Iq = element
                    .try_into()
                    .map_err(|e| XmppError::xml_parse(format!("Invalid iq: {:?}", e)))?;
                // Increment inbound count for SM
                if self.sm_state.enabled {
                    self.sm_state.increment_inbound();
                }
                match self.handle_stanza(Stanza::Iq(iq.clone())).await {
                    Ok(()) => Ok(()),
                    Err(e) => {
                        self.send_iq_error_from_xmpp_error(&iq, &e).await?;
                        Ok(())
                    }
                }
            }
            // XEP-0198 Stream Management stanzas
            ParsedStanza::SmEnable { resume, max } => self.handle_sm_enable(resume, max).await,
            ParsedStanza::SmRequest => self.handle_sm_request().await,
            ParsedStanza::SmAck { h } => self.handle_sm_ack(h).await,
            ParsedStanza::SmResume { previd, h } => self.handle_sm_resume(&previd, h).await,
            // XEP-0352 Client State Indication stanzas
            ParsedStanza::CsiActive => self.handle_csi_active().await,
            ParsedStanza::CsiInactive => self.handle_csi_inactive().await,
            _ => {
                debug!("Ignoring unexpected parsed stanza type");
                Ok(())
            }
        }
    }

    /// Convert an `XmppError` from IQ handling into an IQ error response.
    async fn send_iq_error_from_xmpp_error(
        &mut self,
        iq: &xmpp_parsers::iq::Iq,
        error: &XmppError,
    ) -> Result<(), XmppError> {
        let (condition, error_type, text) = match error {
            XmppError::Stanza {
                condition,
                error_type,
                text,
            } => (*condition, *error_type, text.clone()),
            XmppError::XmlParse(msg) => (
                crate::StanzaErrorCondition::BadRequest,
                crate::StanzaErrorType::Modify,
                Some(msg.clone()),
            ),
            XmppError::PermissionDenied(msg) => (
                crate::StanzaErrorCondition::Forbidden,
                crate::StanzaErrorType::Auth,
                Some(msg.clone()),
            ),
            XmppError::AuthFailed(msg) => (
                crate::StanzaErrorCondition::NotAuthorized,
                crate::StanzaErrorType::Auth,
                Some(msg.clone()),
            ),
            _ => (
                crate::StanzaErrorCondition::InternalServerError,
                crate::StanzaErrorType::Wait,
                Some(error.to_string()),
            ),
        };

        let error_to = iq
            .from
            .as_ref()
            .map(|j| j.to_string())
            .or_else(|| self.jid.as_ref().map(|j| j.to_string()));
        let error_from = iq
            .to
            .as_ref()
            .map(|j| j.to_string())
            .unwrap_or_else(|| self.domain.clone());

        let error_xml = crate::generate_iq_error(
            &iq.id,
            error_to.as_deref(),
            Some(error_from.as_str()),
            condition,
            error_type,
            text.as_deref(),
        );
        self.stream.write_raw(&error_xml).await
    }

    /// Handle XEP-0198 Stream Management enable request.
    async fn handle_sm_enable(&mut self, resume: bool, max: Option<u32>) -> Result<(), XmppError> {
        debug!(resume = resume, max = ?max, "Received SM enable request");

        // Generate a stream ID for potential resumption
        let stream_id = uuid::Uuid::new_v4().to_string();

        // Enable SM with or without resumption
        // For now, we support resumption if requested, with a max timeout of 5 minutes
        let max_seconds = if resume {
            Some(max.unwrap_or(300).min(300))
        } else {
            None
        };

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
        self.stream
            .send_sm_enabled(&stream_id, resume, max_seconds)
            .await?;

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

        debug!(
            h = h,
            previous = self.sm_state.last_acked,
            "Received SM ack from client"
        );
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
                self.isr_token_store
                    .create_token(isr_token.did.clone(), isr_token.jid.clone())
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

        // Standard SM resumption via session registry (XEP-0198)
        match self.sm_session_registry.take_session(previd).await {
            Ok(Some(session)) => {
                info!(
                    stream_id = %previd,
                    jid = %session.jid,
                    "SM session found in registry, resuming"
                );

                // Restore session from detached state
                self.jid = Some(session.jid.clone());
                self.session = Some(Session {
                    did: format!(
                        "did:sm:{}",
                        session
                            .jid
                            .node()
                            .map(|n| n.to_string())
                            .unwrap_or_default()
                    ),
                    jid: session.jid.to_bare().into(),
                    created_at: Utc::now(),
                    expires_at: Utc::now() + chrono::Duration::hours(24),
                });

                // Restore SM state
                self.sm_state.restore_from_session(&session);

                // Get stanzas that need to be resent (client's h tells us what they received)
                let stanzas_to_resend: Vec<_> = session
                    .unacked_stanzas
                    .iter()
                    .filter(|(seq, _)| *seq > h)
                    .map(|(_, xml)| xml.clone())
                    .collect();
                let resent_count = stanzas_to_resend.len();

                // Send resumed response
                self.stream
                    .write_raw(
                        &crate::stream_management::SmResumed::new(
                            previd.to_string(),
                            session.inbound_count,
                        )
                        .to_xml(),
                    )
                    .await?;

                // Resend unacked stanzas
                for stanza_xml in stanzas_to_resend {
                    debug!(
                        stanza_len = stanza_xml.len(),
                        "Resending unacked stanza after resume"
                    );
                    self.stream.write_raw(&stanza_xml).await?;
                }

                // Re-register in connection registry with the restored JID
                let (tx, rx) = mpsc::channel(OUTBOUND_CHANNEL_SIZE);
                self.connection_registry.register(session.jid.clone(), tx);
                self.outbound_rx = Some(rx);

                info!(
                    jid = %session.jid,
                    stream_id = %previd,
                    resent_count = resent_count,
                    "Stream resumed via SM session registry"
                );

                return Ok(());
            }
            Ok(None) => {
                // Session not found or expired
                debug!(previd = %previd, "SM session not found in registry");
            }
            Err(e) => {
                warn!(previd = %previd, error = %e, "Error looking up SM session");
            }
        }

        // No session found via any method
        self.stream
            .send_sm_failed(Some("item-not-found"), None)
            .await?;
        warn!(previd = %previd, "SM resume rejected - session not found");
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
    /// - Chat/Normal: Route to bare/full JID following RFC 6121 priority rules
    /// - Headline: Route to all non-negative available resources of bare JID
    /// - Error: Ignored for server-side routing
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

        // Check for XEP-0249 Direct MUC Invitation (can be in normal or chat messages)
        if let Some(invite) = parse_direct_invite_from_message(&msg) {
            return self.handle_direct_invite(msg, invite, sender_jid).await;
        }

        // Route based on message type
        match msg.type_ {
            MessageType::Groupchat => {
                if let Some(to_jid) = &msg.to {
                    let to_bare = to_jid.to_bare();
                    if self.room_registry.is_muc_jid(&to_bare) {
                        return self.handle_groupchat_message(msg, sender_jid).await;
                    }

                    // RFC 6121 Section 8.5.2.1.1: groupchat to bare JID (non-MUC) MUST error.
                    if to_jid.clone().try_into_full().is_err() {
                        self.send_message_error(
                            &msg,
                            crate::StanzaErrorCondition::ServiceUnavailable,
                            crate::StanzaErrorType::Cancel,
                            Some("groupchat is only valid for MUC destinations"),
                        )
                        .await?;
                        return Ok(());
                    }
                }

                // Full JID groupchat messages are treated as direct messages.
                self.handle_direct_message(msg, sender_jid, BareMessageDelivery::AllNonNegative)
                    .await
            }
            MessageType::Chat => {
                self.handle_direct_message(msg, sender_jid, BareMessageDelivery::HighestOrAll)
                    .await
            }
            MessageType::Normal => {
                self.handle_direct_message(msg, sender_jid, BareMessageDelivery::HighestOrAll)
                    .await
            }
            MessageType::Headline => {
                self.handle_direct_message(msg, sender_jid, BareMessageDelivery::AllNonNegative)
                    .await
            }
            MessageType::Error => {
                let to_full = msg
                    .to
                    .as_ref()
                    .and_then(|jid| jid.clone().try_into_full().ok());

                if to_full.is_some() {
                    // RFC 6121: full-JID delivery for message type='error' is supported.
                    self.handle_direct_message(msg, sender_jid, BareMessageDelivery::AllNonNegative)
                        .await
                } else {
                    // RFC 6121 Section 8.5.2.1.1: bare-JID delivery of message type='error'
                    // MUST NOT be delivered to client resources.
                    debug!(from = %sender_jid, to = ?msg.to, "Dropping bare-JID message type='error'");
                    Ok(())
                }
            }
        }
    }

    fn select_bare_message_targets(
        &self,
        recipient_bare: &jid::BareJid,
        delivery: BareMessageDelivery,
    ) -> Vec<FullJid> {
        let mut available = self
            .connection_registry
            .get_available_resources_for_user(recipient_bare)
            .into_iter()
            .filter(|(_, priority)| *priority >= 0)
            .collect::<Vec<_>>();

        if available.is_empty() {
            return Vec::new();
        }

        available.sort_by(|a, b| a.0.to_string().cmp(&b.0.to_string()));

        match delivery {
            BareMessageDelivery::AllNonNegative => {
                available.into_iter().map(|(jid, _)| jid).collect()
            }
            BareMessageDelivery::HighestOrAll => {
                let highest = available
                    .iter()
                    .map(|(_, priority)| *priority)
                    .max()
                    .unwrap_or(0);
                let highest_resources = available
                    .iter()
                    .filter(|(_, priority)| *priority == highest)
                    .count();

                // RFC 6121 allows delivery to either exactly one highest-priority
                // resource or all non-negative resources. We choose:
                // - all non-negative when all share highest priority
                // - exactly one highest-priority resource otherwise.
                if highest_resources == available.len() {
                    available.into_iter().map(|(jid, _)| jid).collect()
                } else {
                    available
                        .into_iter()
                        .find(|(_, priority)| *priority == highest)
                        .map(|(jid, _)| vec![jid])
                        .unwrap_or_default()
                }
            }
        }
    }

    async fn send_message_error(
        &mut self,
        original: &xmpp_parsers::message::Message,
        condition: crate::StanzaErrorCondition,
        error_type: crate::StanzaErrorType,
        text: Option<&str>,
    ) -> Result<(), XmppError> {
        let mut xml = "<message type='error'".to_string();

        if let Some(to) = &original.from {
            xml.push_str(&format!(" to='{}'", to));
        }

        if let Some(from) = &original.to {
            xml.push_str(&format!(" from='{}'", from));
        }

        if let Some(id) = &original.id {
            xml.push_str(&format!(" id='{}'", id));
        }

        xml.push_str(&format!(
            "><error type='{}'><{} xmlns='{}'/>{}</error></message>",
            error_type.as_str(),
            condition.as_str(),
            crate::parser::ns::STANZAS,
            text.map(|t| format!(
                "<text xmlns='{}' xml:lang='en'>{}</text>",
                crate::parser::ns::STANZAS,
                t
            ))
            .unwrap_or_default()
        ));

        self.stream.write_raw(&xml).await
    }

    /// Handle a groupchat (MUC) message.
    ///
    /// Routes the message to the appropriate MUC room, which broadcasts
    /// it to all occupants (including sending an echo back to the sender
    /// per XEP-0045). Also archives the message to MAM storage.
    ///
    /// ## Federation Support
    ///
    /// When `stanza_router` is configured and federation is enabled, messages
    /// are routed via the federated broadcast mechanism:
    /// - Local occupants receive messages via ConnectionRegistry (C2S)
    /// - Remote occupants receive messages via S2S federation
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

        // Read the room and broadcast the message using federated routing
        let room = room_data.read().await;

        // Find the sender's nick in the room and verify permissions
        let sender_occupant = room.find_occupant_by_real_jid(&sender_jid).ok_or_else(|| {
            debug!(
                sender = %sender_jid,
                room = %room_jid,
                "Sender is not an occupant of the room"
            );
            XmppError::forbidden(Some(format!("You are not an occupant of {}", room_jid)))
        })?;

        let sender_nick = sender_occupant.nick.clone();

        // Check if sender has permission to speak (XEP-0045: visitors cannot speak in moderated rooms)
        if room.config.moderated && sender_occupant.role == Role::Visitor {
            return Err(XmppError::forbidden(Some(
                "Visitors cannot speak in moderated rooms".to_string(),
            )));
        }

        // Use federated broadcast to get messages grouped by delivery target
        let federated_messages = room.broadcast_message_federated(&sender_nick, &muc_msg.message);

        // Capture counts before consuming the FederatedMessageSet
        let local_count = federated_messages.local_count();
        let remote_domain_count = federated_messages.remote_domain_count();
        let remote_count = federated_messages.remote_count();

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

        // Route local messages via ConnectionRegistry
        for mut outbound in federated_messages.local {
            // Add stanza-id if we archived successfully
            if let Some(ref archive_id) = archive_id {
                add_stanza_id(&mut outbound.message, archive_id, &room_jid.to_string());
            }

            if outbound.to == sender_jid {
                // This is the echo back to the sender - write directly to our stream
                debug!(to = %outbound.to, "Sending message echo to sender");
                self.stream
                    .write_stanza(&Stanza::Message(outbound.message.clone()))
                    .await?;
            } else {
                // Route to other local occupants via the connection registry
                let stanza = Stanza::Message(outbound.message.clone());
                let result = self.connection_registry.send_to(&outbound.to, stanza).await;

                match result {
                    SendResult::Sent => {
                        debug!(to = %outbound.to, "Message routed to local occupant");
                    }
                    SendResult::NotConnected => {
                        debug!(
                            to = %outbound.to,
                            "Local occupant not connected, message not delivered"
                        );
                    }
                    SendResult::ChannelFull => {
                        warn!(
                            to = %outbound.to,
                            "Local occupant's channel full, message dropped"
                        );
                    }
                    SendResult::ChannelClosed => {
                        debug!(
                            to = %outbound.to,
                            "Local occupant's channel closed, message not delivered"
                        );
                    }
                }
            }
        }

        // Route remote messages via S2S when federation is enabled
        if !federated_messages.remote.is_empty() {
            if let Some(ref router) = self.stanza_router {
                if router.is_federation_enabled() {
                    for (domain, messages) in federated_messages.remote {
                        debug!(
                            domain = %domain,
                            message_count = messages.len(),
                            room = %room_jid,
                            "Routing messages to remote occupants via S2S"
                        );

                        for mut outbound in messages {
                            // Add stanza-id if we archived successfully
                            if let Some(ref archive_id) = archive_id {
                                add_stanza_id(
                                    &mut outbound.message,
                                    archive_id,
                                    &room_jid.to_string(),
                                );
                            }

                            // Route via the stanza router which handles S2S
                            match router
                                .route_message(outbound.message.clone(), &sender_jid)
                                .await
                            {
                                Ok(result) => {
                                    debug!(
                                        to = %outbound.to,
                                        result = ?result,
                                        "Message routed to remote occupant"
                                    );
                                }
                                Err(e) => {
                                    warn!(
                                        to = %outbound.to,
                                        error = %e,
                                        "Failed to route message to remote occupant"
                                    );
                                }
                            }
                        }
                    }
                } else {
                    debug!(
                        room = %room_jid,
                        remote_domain_count = remote_domain_count,
                        "Federation disabled, skipping remote occupant delivery"
                    );
                }
            } else {
                debug!(
                    room = %room_jid,
                    remote_domain_count = remote_domain_count,
                    "No stanza router configured, skipping remote occupant delivery"
                );
            }
        }

        debug!(
            room = %room_jid,
            local_count = local_count,
            remote_domain_count = remote_domain_count,
            remote_count = remote_count,
            archived = archive_id.is_some(),
            "Groupchat message processed with federated routing"
        );

        Ok(())
    }

    /// Handle a direct non-groupchat message (chat/normal/headline).
    ///
    /// For bare-JID recipients, delivery follows RFC 6121 non-negative priority rules.
    /// For full-JID recipients, delivery targets the exact addressed resource.
    #[instrument(skip(self, msg), fields(to = ?msg.to, msg_type = ?msg.type_))]
    async fn handle_direct_message(
        &mut self,
        msg: xmpp_parsers::message::Message,
        sender_jid: FullJid,
        bare_delivery: BareMessageDelivery,
    ) -> Result<(), XmppError> {
        let recipient_jid = match &msg.to {
            Some(jid) => jid.clone(),
            None => {
                warn!("Direct message missing 'to' attribute");
                return Err(XmppError::bad_request(Some(
                    "Message must have a recipient".to_string(),
                )));
            }
        };

        debug!(
            sender = %sender_jid,
            recipient = %recipient_jid,
            msg_type = ?msg.type_,
            "Routing direct message"
        );

        // Ensure the message has the sender's full JID
        let mut msg_with_from = msg.clone();
        msg_with_from.from = Some(sender_jid.clone().into());

        // Determine if this message should be carbon-copied
        let should_carbon = msg.type_ == MessageType::Chat && should_copy_message(&msg);

        let sender_bare = sender_jid.to_bare();
        let recipient_bare = recipient_jid.to_bare();

        // Respect XEP-0191: silently drop messages to recipients who have blocked the sender
        let is_blocked = self
            .app_state
            .is_blocked(&recipient_bare, &sender_bare)
            .await?;
        if is_blocked {
            debug!(
                sender = %sender_bare,
                recipient = %recipient_bare,
                "Recipient has blocked sender; dropping direct message"
            );
            if should_carbon {
                self.send_sent_carbons(&msg_with_from).await;
            }
            return Ok(());
        }

        let recipient_full = recipient_jid.clone().try_into_full().ok();
        let mut delivered_resources = Vec::new();
        let mut delivered = false;

        if let Some(recipient_full) = recipient_full {
            let stanza = Stanza::Message(msg_with_from.clone());
            let mut should_fallback_to_bare = false;

            match self
                .connection_registry
                .send_to(&recipient_full, stanza)
                .await
            {
                SendResult::Sent => {
                    delivered = true;
                    delivered_resources.push(recipient_full.clone());
                    debug!(to = %recipient_full, "Message delivered to full JID recipient");
                }
                SendResult::NotConnected => {
                    debug!(
                        to = %recipient_full,
                        "Recipient full JID is not connected"
                    );
                    should_fallback_to_bare = true;
                }
                SendResult::ChannelFull => {
                    warn!(
                        to = %recipient_full,
                        "Recipient full JID channel full, message dropped"
                    );
                }
                SendResult::ChannelClosed => {
                    debug!(
                        to = %recipient_full,
                        "Recipient full JID channel closed"
                    );
                    should_fallback_to_bare = true;
                }
            }

            // RFC 6121: if a full-JID target is unavailable, only `type='chat'`
            // is rerouted as bare-JID delivery.
            if should_fallback_to_bare && matches!(msg.type_, MessageType::Chat) {
                let fallback_resources =
                    self.select_bare_message_targets(&recipient_bare, bare_delivery);
                for resource_jid in fallback_resources {
                    let stanza = Stanza::Message(msg_with_from.clone());
                    match self.connection_registry.send_to(&resource_jid, stanza).await {
                        SendResult::Sent => {
                            delivered = true;
                            delivered_resources.push(resource_jid.clone());
                            debug!(to = %resource_jid, "Message delivered via bare-JID fallback");
                        }
                        SendResult::NotConnected | SendResult::ChannelClosed => {
                            debug!(to = %resource_jid, "Fallback recipient unavailable");
                        }
                        SendResult::ChannelFull => {
                            warn!(to = %resource_jid, "Fallback recipient channel full");
                        }
                    }
                }
            }
        } else {
            let recipient_resources =
                self.select_bare_message_targets(&recipient_bare, bare_delivery);

            if recipient_resources.is_empty() {
                debug!(
                    recipient = %recipient_bare,
                    "No non-negative available recipient resources for bare-JID routing"
                );
            }

            for resource_jid in &recipient_resources {
                let stanza = Stanza::Message(msg_with_from.clone());
                let result = self.connection_registry.send_to(resource_jid, stanza).await;

                match result {
                    SendResult::Sent => {
                        debug!(to = %resource_jid, "Message delivered to recipient resource");
                        delivered = true;
                        delivered_resources.push(resource_jid.clone());
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
        }

        // Send "received" carbons to recipient's other clients only when a
        // specific full JID was addressed.
        if delivered
            && should_carbon
            && msg
                .to
                .as_ref()
                .and_then(|jid| jid.clone().try_into_full().ok())
                .is_some()
        {
            self.send_received_carbons_to_user(
                &msg_with_from,
                &recipient_bare,
                &delivered_resources,
            )
            .await;
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
            "Direct message processed"
        );

        Ok(())
    }

    /// Handle a XEP-0249 Direct MUC Invitation.
    ///
    /// Routes the invitation message to the recipient's connected resources.
    /// The invitation contains:
    /// - Room JID to join (required)
    /// - Optional reason for the invitation
    /// - Optional password for password-protected rooms
    ///
    /// Per XEP-0249, direct invites are sent as messages with an
    /// `<x xmlns='jabber:x:conference'>` child element.
    #[instrument(skip(self, msg, invite), fields(room = %invite.jid, to = ?msg.to))]
    async fn handle_direct_invite(
        &mut self,
        msg: xmpp_parsers::message::Message,
        invite: DirectInvite,
        sender_jid: FullJid,
    ) -> Result<(), XmppError> {
        let recipient_jid = match &msg.to {
            Some(jid) => jid.clone(),
            None => {
                warn!("Direct invite missing 'to' attribute");
                return Err(XmppError::bad_request(Some(
                    "Direct invite must have a recipient".to_string(),
                )));
            }
        };

        debug!(
            sender = %sender_jid,
            recipient = %recipient_jid,
            room = %invite.jid,
            has_reason = invite.reason.is_some(),
            has_password = invite.password.is_some(),
            "Processing XEP-0249 direct MUC invitation"
        );

        // Ensure the message has the sender's full JID
        let mut msg_with_from = msg.clone();
        msg_with_from.from = Some(sender_jid.clone().into());

        let sender_bare = sender_jid.to_bare();
        let recipient_bare = recipient_jid.to_bare();

        // Respect XEP-0191: silently drop invites to recipients who have blocked the sender
        let is_blocked = self
            .app_state
            .is_blocked(&recipient_bare, &sender_bare)
            .await?;
        if is_blocked {
            debug!(
                sender = %sender_bare,
                recipient = %recipient_bare,
                "Recipient has blocked sender; dropping direct invite"
            );
            return Ok(());
        }

        // Route to all connected resources for the recipient
        let recipient_resources = self
            .connection_registry
            .get_resources_for_user(&recipient_bare);

        let mut delivered = false;

        if recipient_resources.is_empty() {
            debug!(
                recipient = %recipient_bare,
                "Recipient has no connected resources for direct invite"
            );
            // Note: In a full implementation, we might queue for offline delivery.
            // For now, we log this and continue - the invite will be lost if the
            // recipient is offline.
        } else {
            for resource_jid in &recipient_resources {
                let stanza = Stanza::Message(msg_with_from.clone());
                let result = self.connection_registry.send_to(resource_jid, stanza).await;

                match result {
                    SendResult::Sent => {
                        debug!(to = %resource_jid, "Direct invite delivered to recipient resource");
                        delivered = true;
                    }
                    SendResult::NotConnected => {
                        debug!(to = %resource_jid, "Recipient resource not connected for direct invite");
                    }
                    SendResult::ChannelFull => {
                        warn!(to = %resource_jid, "Recipient's channel full, direct invite dropped");
                    }
                    SendResult::ChannelClosed => {
                        debug!(to = %resource_jid, "Recipient's channel closed");
                    }
                }
            }
        }

        debug!(
            sender = %sender_jid,
            recipient = %recipient_jid,
            room = %invite.jid,
            delivered = delivered,
            "Direct MUC invitation processed"
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
            PresenceAction::Probe {
                from,
                to,
                to_was_full,
            } => {
                return self.handle_presence_probe(from, to, to_was_full).await;
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

        // Get the room data, or create an instant room if it doesn't exist
        // Per XEP-0045 Section 10.1.2, rooms can be created dynamically when a user joins
        let (room_data, room_created) = match self.room_registry.get_room_data(&join_req.room_jid) {
            Some(data) => (data, false),
            None => {
                // Create instant room (XEP-0045 Section 10.1.2)
                debug!(
                    room = %join_req.room_jid,
                    creator = %join_req.sender_jid,
                    "Creating instant room on first join"
                );
                self.room_registry
                    .create_instant_room(join_req.room_jid.clone())?;
                (
                    self.room_registry
                        .get_room_data(&join_req.room_jid)
                        .ok_or_else(|| {
                            XmppError::internal(
                                "Failed to get room data after creation".to_string(),
                            )
                        })?,
                    true,
                )
            }
        };

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
        let resolver =
            AppStateAffiliationResolver::new(Arc::clone(&self.app_state), self.domain.clone());

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

        // For newly created instant rooms, ensure the creator is room owner.
        let effective_affiliation = if room_created {
            Affiliation::Owner
        } else {
            resolved_affiliation
        };

        // Update the room's affiliation list with the effective affiliation
        // This ensures the affiliation is persisted for subsequent queries
        let bare_jid = join_req.sender_jid.to_bare();
        if effective_affiliation != Affiliation::None {
            room.update_affiliation_from_resolver(bare_jid.clone(), effective_affiliation);
            debug!(
                jid = %bare_jid,
                affiliation = %effective_affiliation,
                "Updated room affiliation"
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
        let new_occupant = room.add_occupant_with_affiliation(
            join_req.sender_jid.clone(),
            join_req.nick.clone(),
            Some(self.domain.as_str()),
        );
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
        for (existing_jid, existing_nick, existing_affiliation, existing_role) in
            &existing_occupants
        {
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
                false,              // not self
                Some(existing_jid), // real JID for semi-anonymous rooms
            );

            self.stream
                .write_stanza(&Stanza::Presence(presence))
                .await?;
        }

        // Send room history to the joining user (XEP-0045 §7.2.15)
        // History comes after other occupants' presence but before self-presence
        self.send_muc_history(&join_req).await;

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

        self.stream
            .write_stanza(&Stanza::Presence(self_presence))
            .await?;

        info!(
            room = %join_req.room_jid,
            nick = %join_req.nick,
            occupant_count = occupant_count,
            "User joined MUC room"
        );

        Ok(())
    }

    /// Send room history to a joining user (XEP-0045 §7.2.15).
    ///
    /// Queries MAM storage for recent messages and sends them as
    /// groupchat messages with delay timestamps.
    async fn send_muc_history(&mut self, join_req: &MucJoinRequest) {
        // Get history parameters from the join request, or use defaults
        let history = join_req
            .history
            .as_ref()
            .cloned()
            .unwrap_or_else(crate::muc::HistoryRequest::default_request);

        // Check if history is disabled (maxchars=0 or maxstanzas=0)
        if history.is_disabled() {
            debug!(room = %join_req.room_jid, "History disabled by client");
            return;
        }

        // Build MAM query based on history request
        let max = history.maxstanzas.unwrap_or(25).min(100); // Cap at 100
        let start = if let Some(seconds) = history.seconds {
            Some(chrono::Utc::now() - chrono::Duration::seconds(seconds as i64))
        } else {
            history.since
        };

        let mam_query = crate::mam::MamQuery {
            start,
            max: Some(max),
            ..Default::default()
        };

        // Query MAM storage
        let room_jid_str = join_req.room_jid.to_string();
        match self
            .mam_storage
            .query_messages(&room_jid_str, &mam_query)
            .await
        {
            Ok(result) => {
                debug!(
                    room = %join_req.room_jid,
                    message_count = result.messages.len(),
                    "Sending room history to joining user"
                );

                for archived_msg in result.messages {
                    // Build a history message with delay stamp
                    if let Err(e) = self
                        .send_history_message(
                            &join_req.room_jid,
                            &join_req.sender_jid,
                            &archived_msg,
                        )
                        .await
                    {
                        warn!(
                            room = %join_req.room_jid,
                            error = %e,
                            "Failed to send history message"
                        );
                    }
                }
            }
            Err(e) => {
                warn!(
                    room = %join_req.room_jid,
                    error = %e,
                    "Failed to query room history"
                );
            }
        }
    }

    /// Send a single history message to a user.
    async fn send_history_message(
        &mut self,
        room_jid: &jid::BareJid,
        to_jid: &FullJid,
        archived: &crate::mam::ArchivedMessage,
    ) -> Result<(), XmppError> {
        use jid::Jid;
        use minidom::Element;
        use xmpp_parsers::message::{Body, Message, MessageType as MsgType};

        // Delay namespace (XEP-0203)
        const DELAY_NS: &str = "urn:xmpp:delay";

        // Build the from JID (room@domain/sender_nick)
        // The 'from' in archived message is typically the full room JID with nick
        let from_jid: Jid = archived
            .from
            .parse()
            .unwrap_or_else(|_| Jid::from(room_jid.clone()));

        // Create the history message
        let mut message = Message::new(Some(Jid::from(to_jid.clone())));
        message.type_ = MsgType::Groupchat;
        message.from = Some(from_jid);
        message.id = Some(archived.id.clone());
        message
            .bodies
            .insert(String::new(), Body(archived.body.clone()));

        // Add delay element per XEP-0203
        let delay = Element::builder("delay", DELAY_NS)
            .attr("stamp", archived.timestamp.to_rfc3339())
            .attr("from", room_jid.to_string())
            .build();
        message.payloads.push(delay);

        self.stream.write_stanza(&Stanza::Message(message)).await
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
        let room_data = self
            .room_registry
            .get_room_data(&leave_req.room_jid)
            .ok_or_else(|| {
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
        let occupant = room
            .get_occupant(&nick)
            .ok_or_else(|| XmppError::internal("Occupant disappeared during leave".to_string()))?;
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

        self.stream
            .write_stanza(&Stanza::Presence(self_presence))
            .await?;

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
            SubscriptionType::Subscribe => self.handle_outbound_subscribe(request).await,
            SubscriptionType::Subscribed => self.handle_outbound_subscribed(request).await,
            SubscriptionType::Unsubscribe => self.handle_outbound_unsubscribe(request).await,
            SubscriptionType::Unsubscribed => self.handle_outbound_unsubscribed(request).await,
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

        // Create or update roster item for the contact while preserving metadata
        // such as existing display name and groups.
        let mut item = self
            .app_state
            .get_roster_item(&user_jid, &contact_jid)
            .await?
            .unwrap_or_else(|| RosterItem::new(contact_jid.clone()));
        SubscriptionStateMachine::apply_outbound_subscribe(&mut item);
        let _ = self.app_state.set_roster_item(&user_jid, &item).await?;

        // Send roster push to user's connected resources
        self.send_roster_push_for_user(&user_jid, &item).await;

        // Build and route the subscribe presence to the contact
        let subscribe_pres = build_subscription_presence(
            SubscriptionType::Subscribe,
            &user_jid,
            &contact_jid,
            request.status.as_deref(),
            &request.payloads,
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

        // Update approver's roster (contact receives approver's presence).
        let mut item = self
            .app_state
            .get_roster_item(&user_jid, &contact_jid)
            .await?
            .unwrap_or_else(|| RosterItem::new(contact_jid.clone()));
        SubscriptionStateMachine::apply_outbound_subscribed(&mut item);
        let _ = self.app_state.set_roster_item(&user_jid, &item).await?;

        // Send roster push to user's connected resources
        self.send_roster_push_for_user(&user_jid, &item).await;

        // Update requester's roster as inbound approved subscription and push.
        let mut contact_item = self
            .app_state
            .get_roster_item(&contact_jid, &user_jid)
            .await?
            .unwrap_or_else(|| RosterItem::new(user_jid.clone()));
        SubscriptionStateMachine::apply_inbound_subscribed(&mut contact_item);
        let _ = self
            .app_state
            .set_roster_item(&contact_jid, &contact_item)
            .await?;
        self.send_roster_push_for_user(&contact_jid, &contact_item)
            .await;

        // Build and route the subscribed presence to the contact
        let subscribed_pres = build_subscription_presence(
            SubscriptionType::Subscribed,
            &user_jid,
            &contact_jid,
            None,
            &request.payloads,
        );

        let stanza = Stanza::Presence(subscribed_pres);
        self.route_stanza_to_bare_jid(&contact_jid, stanza).await?;

        // Send current presence to the newly subscribed contact
        // (they're now allowed to receive our presence)
        if let Some(ref jid) = self.jid {
            let available_pres = if let Some(mut current) = self.last_available_presence.clone() {
                current.from = Some(jid.clone().into());
                current.to = Some(contact_jid.clone().into());
                current
            } else {
                build_available_presence(
                    jid,
                    &contact_jid,
                    None, // show
                    None, // status
                    0,    // priority
                )
            };
            let stanza = Stanza::Presence(available_pres);
            self.route_stanza_to_bare_jid(&contact_jid, stanza).await?;
        }

        // Request the requester's latest presence state so the approver receives
        // current availability/status immediately after approval.
        let mut probe = xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Probe);
        probe.from = Some(user_jid.clone().into());
        probe.to = Some(contact_jid.clone().into());
        self.route_stanza_to_bare_jid(&contact_jid, Stanza::Presence(probe))
            .await?;

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

        // Update roster item with new subscription state, preserving metadata.
        let mut item = self
            .app_state
            .get_roster_item(&user_jid, &contact_jid)
            .await?
            .unwrap_or_else(|| RosterItem::new(contact_jid.clone()));
        SubscriptionStateMachine::apply_outbound_unsubscribe(&mut item);
        let _ = self.app_state.set_roster_item(&user_jid, &item).await?;

        // Send roster push to user's connected resources
        self.send_roster_push_for_user(&user_jid, &item).await;

        // Build and route the unsubscribe presence to the contact
        let unsubscribe_pres = build_subscription_presence(
            SubscriptionType::Unsubscribe,
            &user_jid,
            &contact_jid,
            None,
            &request.payloads,
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

        // Update sender-side roster item with new subscription state.
        let mut item = self
            .app_state
            .get_roster_item(&user_jid, &contact_jid)
            .await?
            .unwrap_or_else(|| RosterItem::new(contact_jid.clone()));
        SubscriptionStateMachine::apply_outbound_unsubscribed(&mut item);
        let _ = self.app_state.set_roster_item(&user_jid, &item).await?;

        // Send roster push to user's connected resources
        self.send_roster_push_for_user(&user_jid, &item).await;

        // Update contact-side roster item as inbound unsubscribed and push.
        let mut contact_item = self
            .app_state
            .get_roster_item(&contact_jid, &user_jid)
            .await?
            .unwrap_or_else(|| RosterItem::new(user_jid.clone()));
        SubscriptionStateMachine::apply_inbound_unsubscribed(&mut contact_item);
        let _ = self
            .app_state
            .set_roster_item(&contact_jid, &contact_item)
            .await?;
        self.send_roster_push_for_user(&contact_jid, &contact_item)
            .await;

        // Build and route the unsubscribed presence to the contact
        let unsubscribed_pres = build_subscription_presence(
            SubscriptionType::Unsubscribed,
            &user_jid,
            &contact_jid,
            None,
            &request.payloads,
        );

        let stanza = Stanza::Presence(unsubscribed_pres);
        self.route_stanza_to_bare_jid(&contact_jid, stanza).await?;

        // Send unavailable presence to the contact (they can no longer see us)
        let unavailable_pres = build_unavailable_presence(&user_jid, &contact_jid);
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
        _to_was_full: bool,
    ) -> Result<(), XmppError> {
        debug!(
            from = %from,
            to = %to,
            "Processing presence probe"
        );

        // Check if the requesting user has a subscription that allows them
        // to receive the target's presence (subscription=to or both)
        // For now, we'll respond with current presence if the target is connected

        // If target is a local bare JID and the account does not exist,
        // respond with <presence type='unsubscribed'> per RFC 6121 §4.3.
        if to.domain().as_str() == self.domain {
            if !self.local_user_exists(&to).await? {
                let mut unsubscribed = xmpp_parsers::presence::Presence::new(PresenceType::Unsubscribed);
                unsubscribed.from = Some(to.clone().into());
                unsubscribed.to = Some(from.clone().into());
                self.route_stanza_to_bare_jid(&from, Stanza::Presence(unsubscribed))
                    .await?;

                debug!(
                    from = %from,
                    to = %to,
                    "Sent unsubscribed presence in response to probe (account does not exist)"
                );
                return Ok(());
            }
        }

        // Get all connected resources for the target user
        let resources = self.connection_registry.get_resources_for_user(&to);

        if resources.is_empty() {
            // User exists but is offline: return unavailable presence.
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

        // Build a modified presence with XEP-0153 vCard avatar hash.
        let mut pres_clone = pres.clone();
        let vcard_update =
            crate::xep::xep0153::build_vcard_update_element(self.avatar_hash.as_deref());
        pres_clone.payloads.push(vcard_update);
        pres_clone.from = Some(sender_jid.clone().into());

        // Directed presence (presence with 'to') is routed to the addressed JID(s),
        // not roster subscribers.
        if let Some(target) = pres_clone.to.clone() {
            if let Ok(target_full) = target.clone().try_into_full() {
                let stanza = Stanza::Presence(pres_clone.clone());
                match self.connection_registry.send_to(&target_full, stanza).await {
                    SendResult::Sent => return Ok(()),
                    SendResult::NotConnected | SendResult::ChannelClosed => {
                        // Full-JID target is gone; silently drop.
                        return Ok(());
                    }
                    SendResult::ChannelFull => {
                        warn!(to = %target_full, "Directed presence dropped: recipient channel full");
                        return Ok(());
                    }
                }
            }

            let target_bare = target.to_bare();
            let available_targets = self
                .connection_registry
                .get_available_resources_for_user(&target_bare);
            for (target_full, _) in available_targets {
                // Preserve the original bare-JID 'to' value per RFC 6121.
                let stanza = Stanza::Presence(pres_clone.clone());
                let _ = self.connection_registry.send_to(&target_full, stanza).await;
            }
            return Ok(());
        }

        // Undirected presence updates the sender availability state.
        let is_available = matches!(pres.type_, xmpp_parsers::presence::Type::None);
        let priority = pres.priority;
        let _ = self
            .connection_registry
            .update_presence(&sender_jid, is_available, priority);

        if is_available {
            self.last_available_presence = Some(pres_clone.clone());
            self.deliver_pending_subscription_stanzas(&sender_bare)
                .await;
            info!(
                sender = %sender_bare,
                avatar_hash = ?self.avatar_hash,
                priority = priority,
                "User sent available presence"
            );
        } else if matches!(pres.type_, xmpp_parsers::presence::Type::Unavailable) {
            self.last_available_presence = None;
            info!(sender = %sender_bare, "User sent unavailable presence");
        }

        // Broadcast undirected presence to roster subscribers.
        let subscribers = self
            .app_state
            .get_presence_subscribers(&sender_bare)
            .await?;
        for subscriber in subscribers {
            let resources = self
                .connection_registry
                .get_available_resources_for_user(&subscriber);
            for (resource_jid, _) in resources {
                let mut routed = pres_clone.clone();
                routed.to = Some(resource_jid.clone().into());
                let stanza = Stanza::Presence(routed);
                let _ = self
                    .connection_registry
                    .send_to(&resource_jid, stanza)
                    .await;
            }
        }

        Ok(())
    }

    /// Deliver queued subscription stanzas for a user that just became available.
    async fn deliver_pending_subscription_stanzas(&self, user_bare: &jid::BareJid) {
        let pending = self
            .connection_registry
            .drain_pending_subscription_stanzas(user_bare);
        if pending.is_empty() {
            return;
        }

        let resources = self
            .connection_registry
            .get_available_resources_for_user(user_bare);
        if resources.is_empty() {
            // User became unavailable again before we could deliver; re-queue stanzas.
            for stanza in pending {
                self.connection_registry
                    .queue_pending_subscription_stanza(user_bare, stanza);
            }
            return;
        }

        debug!(
            user = %user_bare,
            stanza_count = pending.len(),
            resource_count = resources.len(),
            "Delivering queued subscription stanzas"
        );

        for stanza in pending {
            for (resource_jid, _) in &resources {
                let _ = self
                    .connection_registry
                    .send_to(resource_jid, stanza.clone())
                    .await;
            }
        }
    }

    fn is_queueable_subscription_stanza(stanza: &Stanza) -> bool {
        matches!(
            stanza,
            Stanza::Presence(pres)
                if matches!(
                    pres.type_,
                    PresenceType::Subscribe
                        | PresenceType::Subscribed
                        | PresenceType::Unsubscribe
                        | PresenceType::Unsubscribed
                )
        )
    }

    /// Route a stanza to a bare JID (all connected resources or queued for offline delivery).
    ///
    /// Subscription presence stanzas are queued when the target user is offline and
    /// delivered when the user next becomes available.
    async fn route_stanza_to_bare_jid(
        &self,
        target: &jid::BareJid,
        stanza: Stanza,
    ) -> Result<(), XmppError> {
        // Get all connected resources for the target.
        let resources = self.connection_registry.get_resources_for_user(target);

        if resources.is_empty() {
            if Self::is_queueable_subscription_stanza(&stanza) {
                self.connection_registry
                    .queue_pending_subscription_stanza(target, stanza);
                debug!(
                    target = %target,
                    "Queued subscription stanza for offline user"
                );
            } else {
                debug!(
                    target = %target,
                    "Target user offline, stanza not delivered"
                );
            }
            return Ok(());
        }

        // Send to all connected resources.
        for resource_jid in resources {
            let _ = self
                .connection_registry
                .send_to(&resource_jid, stanza.clone())
                .await;
        }

        Ok(())
    }

    /// Route an IQ addressed to a local full JID.
    ///
    /// Per RFC 6121 Section 8.5, IQ requests to an unavailable full JID MUST
    /// receive a `service-unavailable` error from the addressed full JID.
    async fn route_local_full_jid_iq(
        &mut self,
        mut iq: xmpp_parsers::iq::Iq,
        target_full_jid: FullJid,
    ) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => {
                warn!("IQ received before JID bound");
                return Err(XmppError::not_authorized(Some(
                    "Session not established".to_string(),
                )));
            }
        };

        // Enforce sender identity from the authenticated stream.
        iq.from = Some(sender_jid.clone().into());
        iq.to = Some(target_full_jid.clone().into());

        let stanza = Stanza::Iq(iq.clone());
        match self
            .connection_registry
            .send_to(&target_full_jid, stanza)
            .await
        {
            SendResult::Sent => {
                debug!(
                    from = %sender_jid,
                    to = %target_full_jid,
                    iq_id = %iq.id,
                    "Routed IQ stanza to local full JID"
                );
                Ok(())
            }
            SendResult::NotConnected | SendResult::ChannelClosed | SendResult::ChannelFull => {
                match &iq.payload {
                    IqType::Get(_) | IqType::Set(_) => {
                        let error_to = sender_jid.to_string();
                        let error_from = target_full_jid.to_string();
                        let error = crate::generate_iq_error(
                            &iq.id,
                            Some(error_to.as_str()),
                            Some(error_from.as_str()),
                            crate::StanzaErrorCondition::ServiceUnavailable,
                            crate::StanzaErrorType::Cancel,
                            None,
                        );
                        self.stream.write_raw(&error).await?;
                    }
                    IqType::Result(_) | IqType::Error(_) => {
                        debug!(
                            to = %target_full_jid,
                            iq_id = %iq.id,
                            "Dropping IQ result/error for unavailable local full JID"
                        );
                    }
                }
                Ok(())
            }
        }
    }

    /// Handle an IQ stanza.
    ///
    /// Currently supports:
    /// - disco#info queries (XEP-0030)
    /// - disco#items queries (XEP-0030)
    /// - MAM queries (XEP-0313)
    #[instrument(skip(self, iq), fields(iq_type = ?iq.payload, iq_id = %iq.id))]
    async fn handle_iq(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        // Route IQ stanzas addressed to a local full JID (user@domain/resource).
        // These are client-to-client IQs and must not be handled as server IQs.
        if let Some(to_jid) = &iq.to {
            if to_jid.domain().as_str() == self.domain
                && to_jid.node().is_some()
                && to_jid.resource().is_some()
            {
                if let Ok(full_jid) = to_jid.clone().try_into_full() {
                    return self.route_local_full_jid_iq(iq, full_jid).await;
                }
            }
        }

        // IQ result/error stanzas addressed to the server are terminal and should
        // not trigger additional error responses.
        if matches!(&iq.payload, IqType::Result(_) | IqType::Error(_)) {
            debug!(iq_id = %iq.id, to = ?iq.to, "Ignoring IQ result/error stanza");
            return Ok(());
        }

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

        // Check if this is a ping request (XEP-0199)
        if is_ping(&iq) {
            return self.handle_ping(iq).await;
        }

        // Check if this is an external service discovery request (XEP-0215)
        if matches!(
            &iq.payload,
            IqType::Get(elem) if elem.name() == "services" && elem.ns() == "urn:xmpp:extdisco:2"
        ) {
            return self.handle_external_services_query(iq).await;
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
        // Route upload IQs addressed to upload.{domain} to the upload handler
        if is_upload_request(&iq) {
            return self.handle_upload_slot_request(iq).await;
        }

        // Route PubSub IQs addressed to pubsub.{domain} to the pubsub handler
        {
            let pubsub_domain = format!("pubsub.{}", self.domain);
            if is_pubsub_iq(&iq)
                && iq
                    .to
                    .as_ref()
                    .map(|j| j.to_bare().to_string() == pubsub_domain)
                    .unwrap_or(false)
            {
                return self.handle_pubsub_iq(iq).await;
            }
        }

        // Check if this is a MUC owner IQ (XEP-0045 §10.1-10.2, room config/destroy)
        let muc_domain = self.room_registry.muc_domain();
        if (is_muc_owner_get(&iq) || is_muc_owner_set(&iq))
            && iq
                .to
                .as_ref()
                .map(|j| j.to_bare().domain().as_str() == muc_domain)
                .unwrap_or(false)
        {
            return self.handle_muc_owner_iq(iq).await;
        }

        // Check if this is a MUC admin IQ (XEP-0045 §10, affiliation/role changes)
        if is_muc_admin_iq(&iq, muc_domain) {
            return self.handle_muc_admin_iq(iq).await;
        }

        // Check if this is a private XML storage query (XEP-0049)
        if is_private_storage_query(&iq) {
            return self.handle_private_storage(iq).await;
        }

        // Check if this is a blocking query (XEP-0191)
        if is_blocking_query(&iq) {
            return self.handle_blocking_query(iq).await;
        }

        // Check if this is a PubSub/PEP IQ (XEP-0060/XEP-0163)
        if is_pubsub_iq(&iq) {
            return self.handle_pubsub_iq(iq).await;
        }

        // Unhandled IQ get/set - RFC 6120 §8.2.3 requires an error response
        debug!("Received unhandled IQ stanza, returning service-unavailable");
        let error_to = iq
            .from
            .as_ref()
            .map(|j| j.to_string())
            .or_else(|| self.jid.as_ref().map(|j| j.to_string()));
        let error_from = iq
            .to
            .as_ref()
            .map(|j| j.to_string())
            .unwrap_or_else(|| self.domain.clone());
        let error = crate::generate_iq_error(
            &iq.id,
            error_to.as_deref(),
            Some(error_from.as_str()),
            crate::StanzaErrorCondition::ServiceUnavailable,
            crate::StanzaErrorType::Cancel,
            None,
        );
        self.stream.write_raw(&error).await?;
        Ok(())
    }

    /// Check if a local bare JID corresponds to an existing local user account.
    async fn local_user_exists(&self, bare_jid: &jid::BareJid) -> Result<bool, XmppError> {
        if bare_jid.domain().as_str() != self.domain {
            return Ok(false);
        }

        // Online users are known to exist.
        if !self
            .connection_registry
            .get_resources_for_user(bare_jid)
            .is_empty()
        {
            return Ok(true);
        }

        let Some(node) = bare_jid.node() else {
            return Ok(false);
        };

        self.app_state.native_user_exists(node.as_str()).await
    }

    /// Handle a disco#info query.
    ///
    /// Returns identity and supported features for:
    /// - Server domain: Server identity + server features
    /// - MUC domain: Conference service identity + MUC features
    /// - MUC room: Conference room identity + room features
    /// - Upload domain: Upload service identity + upload features
    /// - PubSub domain: PubSub service identity + PubSub features
    /// - Bare JID: PEP identity + PEP features
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_disco_info_query(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        let query = parse_disco_info_query(&iq)?;

        let muc_domain = self.room_registry.muc_domain();
        let upload_domain = format!("upload.{}", self.domain);
        let pubsub_domain = format!("pubsub.{}", self.domain);

        // Determine what entity is being queried
        let (identities, features, extensions) = match query.target.as_deref() {
            // Query to server domain
            Some(target) if target == self.domain => {
                debug!(domain = %self.domain, "disco#info query to server domain");
                (
                    vec![Identity::server(Some("Waddle XMPP Server"))],
                    server_features(),
                    vec![build_server_info_abuse_form(&self.domain)],
                )
            }
            // Query to MUC domain
            Some(target) if target == muc_domain => {
                debug!(domain = %muc_domain, "disco#info query to MUC domain");
                (
                    vec![Identity::muc_service(Some("Multi-User Chat"))],
                    muc_service_features(),
                    vec![],
                )
            }
            // Query to upload service domain (XEP-0363)
            Some(target) if target == upload_domain => {
                debug!(domain = %upload_domain, "disco#info query to upload domain");
                (
                    vec![Identity::upload_service(Some("HTTP File Upload"))],
                    upload_service_features(),
                    vec![],
                )
            }
            // Query to pubsub service domain (XEP-0060)
            Some(target) if target == pubsub_domain => {
                debug!(domain = %pubsub_domain, "disco#info query to pubsub domain");
                (
                    vec![Identity::pubsub_service(Some("Publish-Subscribe"))],
                    pubsub_service_features(),
                    vec![],
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
                        vec![],
                    )
                } else {
                    // Room doesn't exist
                    return Err(XmppError::item_not_found(Some(format!(
                        "Room {} not found",
                        room_jid
                    ))));
                }
            }
            // Query to a bare JID (user@domain) - PEP service (XEP-0163)
            Some(target) if target.contains('@') && !target.contains('/') => {
                let requester = self
                    .jid
                    .as_ref()
                    .map(|jid| jid.to_bare())
                    .ok_or_else(|| {
                        XmppError::not_authorized(Some("Session not established".to_string()))
                    })?;

                let target_bare: jid::BareJid = target.parse().map_err(|e| {
                    XmppError::bad_request(Some(format!("Invalid bare JID '{}': {}", target, e)))
                })?;

                if target_bare.domain().as_str() != self.domain {
                    return Err(XmppError::service_unavailable(Some(format!(
                        "Unsupported disco#info target {}",
                        target_bare
                    ))));
                }

                if !self.local_user_exists(&target_bare).await? {
                    return Err(XmppError::service_unavailable(Some(format!(
                        "Entity {} not found",
                        target_bare
                    ))));
                }

                // Non-self bare-JID queries without a node are not supported
                if requester != target_bare && query.node.is_none() {
                    return Err(XmppError::service_unavailable(Some(format!(
                        "disco#info on bare JID {} without node is not supported for other users",
                        target_bare
                    ))));
                }

                if requester != target_bare {
                    let has_presence_access = self
                        .app_state
                        .get_roster_item(&target_bare, &requester)
                        .await?
                        .map(|item| {
                            matches!(item.subscription, Subscription::From | Subscription::Both)
                        })
                        .unwrap_or(false);

                    if !has_presence_access {
                        return Err(XmppError::service_unavailable(Some(format!(
                            "PEP info for {} requires presence subscription",
                            target_bare
                        ))));
                    }
                }

                debug!(target = %target_bare, requester = %requester, "disco#info query to bare JID (PEP)");
                (
                    vec![crate::pubsub::pep::build_pep_identity()],
                    crate::pubsub::pep::pep_features(),
                    vec![],
                )
            }
            // No target or unknown target - default to server
            None | Some(_) => {
                debug!(target = ?query.target, "disco#info query (defaulting to server)");
                (
                    vec![Identity::server(Some("Waddle XMPP Server"))],
                    server_features(),
                    vec![build_server_info_abuse_form(&self.domain)],
                )
            }
        };

        let response = build_disco_info_response_with_extensions(
            &iq,
            &identities,
            &features,
            query.node.as_deref(),
            &extensions,
        );
        self.stream.write_stanza(&Stanza::Iq(response)).await?;

        debug!("Sent disco#info response");
        Ok(())
    }

    /// Handle a disco#items query.
    ///
    /// Returns available items/services:
    /// - Server domain: Returns MUC, upload, and PubSub service components
    /// - MUC domain: Returns list of available rooms
    /// - PubSub domain: Returns list of nodes
    /// - Bare JID: Returns PEP node list
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_disco_items_query(
        &mut self,
        iq: xmpp_parsers::iq::Iq,
    ) -> Result<(), XmppError> {
        let query = parse_disco_items_query(&iq)?;

        let muc_domain = self.room_registry.muc_domain();
        let upload_domain = format!("upload.{}", self.domain);
        let pubsub_domain = format!("pubsub.{}", self.domain);

        // Determine what entity is being queried
        let items = match query.target.as_deref() {
            // Query to server domain - return all service components
            Some(target) if target == self.domain => {
                debug!(domain = %self.domain, "disco#items query to server domain");
                vec![
                    DiscoItem::muc_service(muc_domain, Some("Multi-User Chat")),
                    DiscoItem::upload_service(&upload_domain, Some("HTTP File Upload")),
                    DiscoItem::pubsub_service(&pubsub_domain, Some("Publish-Subscribe")),
                ]
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
            // Query to upload domain - no sub-items
            Some(target) if target == upload_domain => {
                debug!(domain = %upload_domain, "disco#items query to upload domain");
                vec![]
            }
            // Query to pubsub domain - return node list
            Some(target) if target == pubsub_domain => {
                debug!(domain = %pubsub_domain, "disco#items query to pubsub domain");
                // List all nodes across all users from shared storage
                // For now, return empty - a full implementation would list all public nodes
                vec![]
            }
            // Query to MUC room - return empty list (no sub-items)
            Some(target) if target.ends_with(&format!("@{}", muc_domain)) => {
                debug!(room = %target, "disco#items query to MUC room");
                vec![] // Rooms don't have sub-items
            }
            // Query to a bare JID (user@domain) - PEP node list (XEP-0163)
            Some(target)
                if target.contains('@')
                    && !target.contains('/')
                    && target.ends_with(&format!("@{}", self.domain)) =>
            {
                debug!(target = %target, "disco#items query to bare JID (PEP nodes)");
                if let Ok(target_jid) = target.parse::<jid::BareJid>() {
                    let nodes = self
                        .pubsub_storage
                        .list_nodes(&target_jid)
                        .await
                        .unwrap_or_default();
                    nodes
                        .iter()
                        .map(|node| DiscoItem::pubsub_node(target, node))
                        .collect()
                } else {
                    vec![]
                }
            }
            // No target or unknown target - default to server services
            None | Some(_) => {
                debug!(target = ?query.target, "disco#items query (defaulting to server)");
                vec![
                    DiscoItem::muc_service(muc_domain, Some("Multi-User Chat")),
                    DiscoItem::upload_service(&upload_domain, Some("HTTP File Upload")),
                    DiscoItem::pubsub_service(&pubsub_domain, Some("Publish-Subscribe")),
                ]
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
        let result_messages =
            build_result_messages(&query_id, &sender_jid.to_string(), &result.messages);

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

    /// Handle XEP-0199 Ping (including XEP-0410 MUC Self-Ping).
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_ping(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => {
                warn!("Ping received before JID bound");
                return Err(XmppError::not_authorized(Some(
                    "Session not established".to_string(),
                )));
            }
        };

        // Handle MUC self-ping (XEP-0410): ping to room@muc.domain/nick
        if let Some(to) = &iq.to {
            let muc_domain = self.room_registry.muc_domain();
            let target_bare = to.to_bare();

            if target_bare.domain().as_str() == muc_domain {
                if let Some(nick) = to.resource() {
                    let to_str = iq.from.as_ref().map(|j| j.to_string());
                    let from_str = iq.to.as_ref().map(|j| j.to_string());

                    let room_data = match self.room_registry.get_room_data(&target_bare) {
                        Some(data) => data,
                        None => {
                            let error = crate::generate_iq_error(
                                &iq.id,
                                to_str.as_deref(),
                                from_str.as_deref(),
                                crate::StanzaErrorCondition::ItemNotFound,
                                crate::StanzaErrorType::Cancel,
                                Some("Room not found"),
                            );
                            self.stream.write_raw(&error).await?;
                            return Ok(());
                        }
                    };

                    let room = room_data.read().await;
                    let occupant = match room.get_occupant(nick) {
                        Some(occupant) => occupant,
                        None => {
                            let error = crate::generate_iq_error(
                                &iq.id,
                                to_str.as_deref(),
                                from_str.as_deref(),
                                crate::StanzaErrorCondition::ItemNotFound,
                                crate::StanzaErrorType::Cancel,
                                Some("Occupant not found"),
                            );
                            self.stream.write_raw(&error).await?;
                            return Ok(());
                        }
                    };

                    if occupant.real_jid != sender_jid {
                        let error = crate::generate_iq_error(
                            &iq.id,
                            to_str.as_deref(),
                            from_str.as_deref(),
                            crate::StanzaErrorCondition::Forbidden,
                            crate::StanzaErrorType::Auth,
                            Some("Self-ping only allowed for own occupant"),
                        );
                        self.stream.write_raw(&error).await?;
                        return Ok(());
                    }

                    let response = build_ping_result(&iq);
                    self.stream.write_stanza(&Stanza::Iq(response)).await?;
                    return Ok(());
                }
            }
        }

        // IQ ping to a local bare JID of a non-existing user MUST return an error.
        if let Some(to) = &iq.to {
            if to.domain().as_str() == self.domain && to.node().is_some() && to.resource().is_none()
            {
                let target_bare = to.to_bare();
                if !self.local_user_exists(&target_bare).await? {
                    let error_to = iq.from.as_ref().map(|j| j.to_string());
                    let error_from = iq.to.as_ref().map(|j| j.to_string());
                    let error = crate::generate_iq_error(
                        &iq.id,
                        error_to.as_deref(),
                        error_from.as_deref(),
                        crate::StanzaErrorCondition::ServiceUnavailable,
                        crate::StanzaErrorType::Cancel,
                        None,
                    );
                    self.stream.write_raw(&error).await?;
                    return Ok(());
                }
            }
        }

        // Default ping response (server or component)
        let response = build_ping_result(&iq);
        self.stream.write_stanza(&Stanza::Iq(response)).await?;
        Ok(())
    }

    /// Handle external service discovery query (XEP-0215).
    ///
    /// Returns STUN and TURN service entries from static configuration.
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_external_services_query(
        &mut self,
        iq: xmpp_parsers::iq::Iq,
    ) -> Result<(), XmppError> {
        let stun_host =
            std::env::var("WADDLE_EXTDISCO_STUN_HOST").unwrap_or_else(|_| self.domain.clone());
        let stun_port = std::env::var("WADDLE_EXTDISCO_STUN_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(3478);

        let turn_host =
            std::env::var("WADDLE_EXTDISCO_TURN_HOST").unwrap_or_else(|_| stun_host.clone());
        let turn_port = std::env::var("WADDLE_EXTDISCO_TURN_PORT")
            .ok()
            .and_then(|v| v.parse::<u16>().ok())
            .unwrap_or(3478);
        let turn_username =
            std::env::var("WADDLE_EXTDISCO_TURN_USERNAME").unwrap_or_else(|_| "waddle".to_string());
        let turn_password =
            std::env::var("WADDLE_EXTDISCO_TURN_PASSWORD").unwrap_or_else(|_| "waddle".to_string());

        let services = minidom::Element::builder("services", "urn:xmpp:extdisco:2")
            .append(
                minidom::Element::builder("service", "urn:xmpp:extdisco:2")
                    .attr("type", "stun")
                    .attr("host", stun_host.as_str())
                    .attr("port", stun_port.to_string())
                    .attr("transport", "udp")
                    .build(),
            )
            .append(
                minidom::Element::builder("service", "urn:xmpp:extdisco:2")
                    .attr("type", "turn")
                    .attr("host", turn_host.as_str())
                    .attr("port", turn_port.to_string())
                    .attr("transport", "udp")
                    .attr("username", turn_username.as_str())
                    .attr("password", turn_password.as_str())
                    .build(),
            )
            .build();

        let response = xmpp_parsers::iq::Iq {
            from: iq.to.clone(),
            to: iq.from.clone(),
            id: iq.id.clone(),
            payload: IqType::Result(Some(services)),
        };
        self.stream.write_stanza(&Stanza::Iq(response)).await?;
        Ok(())
    }

    /// Handle PubSub/PEP IQ (XEP-0060/XEP-0163).
    ///
    /// Supports:
    /// - Publish: Publish items to PEP nodes (auto-creates nodes)
    /// - Items: Retrieve items from PEP nodes
    /// - Retract: Delete items from nodes
    /// - Create/Delete: Node management
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_pubsub_iq(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        // Require a bound JID
        let user_jid = match &self.jid {
            Some(jid) => jid.to_bare(),
            None => {
                warn!("PubSub request received before JID bound");
                let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                self.stream.write_stanza(&Stanza::Iq(error)).await?;
                return Ok(());
            }
        };

        // Check if this is a PEP request (to self)
        let target_jid = match &iq.to {
            Some(to_jid) => to_jid.to_bare(),
            None => user_jid.clone(), // Implicit PEP (to self)
        };

        // For now, only support PEP (requests to self)
        // Full PubSub service would require different handling
        if target_jid != user_jid {
            debug!(
                target = %target_jid,
                user = %user_jid,
                "PubSub request to another user's PEP service - access check needed"
            );
            // For reading another user's PEP, we'd need presence subscription checks
            // For now, we'll allow it but only for retrieval operations
        }

        // Parse the PubSub request
        let request = match parse_pubsub_iq(&iq) {
            Ok(req) => req,
            Err(e) => {
                warn!("Failed to parse PubSub request: {}", e);
                let error = build_pubsub_error(&iq, PubSubError::InvalidJid);
                self.stream.write_stanza(&Stanza::Iq(error)).await?;
                return Ok(());
            }
        };

        debug!(?request, "Handling PubSub request");

        match request {
            PubSubRequest::Publish { node, item } => {
                // PEP auto-create: publish with auto_create=true
                let result = self
                    .pubsub_storage
                    .publish_item(
                        &target_jid,
                        &node,
                        &item,
                        Some(&user_jid),
                        true, // auto-create for PEP
                    )
                    .await;

                match result {
                    Ok(publish_result) => {
                        debug!(
                            node = %node,
                            item_id = %publish_result.item_id,
                            created = publish_result.node_created,
                            "PubSub item published"
                        );

                        // Send success response
                        let response =
                            build_pubsub_publish_result(&iq, &node, &publish_result.item_id);
                        self.stream.write_stanza(&Stanza::Iq(response)).await?;

                        // Send event notifications to subscribers (presence-based for PEP)
                        // For now, we don't implement subscription notifications
                        // A full implementation would broadcast to roster contacts

                        // XEP-0398: Convert PEP avatar to vCard PHOTO (if avatar metadata published)
                        if crate::xep::xep0084::is_avatar_metadata_node(&node)
                            && !self.converting_avatar
                        {
                            self.converting_avatar = true;
                            // Get the avatar data from PEP to convert to vCard
                            if let Ok(data_items) = self
                                .pubsub_storage
                                .get_items(
                                    &target_jid,
                                    crate::xep::xep0084::NODE_AVATAR_DATA,
                                    Some(1),
                                    std::slice::from_ref(&publish_result.item_id),
                                )
                                .await
                            {
                                if let Some(data_item) = data_items.first() {
                                    if let Some(ref payload_xml) = data_item.payload_xml {
                                        if let Ok(data_elem) =
                                            payload_xml.parse::<minidom::Element>()
                                        {
                                            if let Some(avatar_data_b64) =
                                                crate::xep::xep0084::parse_avatar_data(&data_elem)
                                            {
                                                // Parse the metadata we just published
                                                if let Some(ref payload) = item.payload {
                                                    if let Some(avatar_info) =
                                                        crate::xep::xep0084::parse_avatar_metadata(
                                                            payload,
                                                        )
                                                    {
                                                        let converter = crate::xep::xep0398::DefaultAvatarConversion;
                                                        if let Some((photo_b64, mime_type)) =
                                                            converter.on_pep_avatar_published(
                                                                &avatar_data_b64,
                                                                &avatar_info,
                                                            )
                                                        {
                                                            // Update vCard with the photo
                                                            let vcard = crate::xep::xep0054::VCard {
                                                                photo: Some(
                                                                    crate::xep::xep0054::VCardPhoto {
                                                                        mime_type,
                                                                        data: photo_b64.clone(),
                                                                    },
                                                                ),
                                                                ..Default::default()
                                                            };
                                                            let vcard_elem = crate::xep::xep0054::build_vcard_element(&vcard);
                                                            let vcard_xml =
                                                                String::from(&vcard_elem);
                                                            let _ = self
                                                                .app_state
                                                                .set_vcard(&user_jid, &vcard_xml)
                                                                .await;

                                                            // Update avatar hash
                                                            self.avatar_hash = crate::xep::xep0153::compute_photo_hash_from_base64(&photo_b64);
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                            self.converting_avatar = false;
                        }
                    }
                    Err(e) => {
                        warn!("PubSub publish failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                        self.stream.write_stanza(&Stanza::Iq(error)).await?;
                    }
                }
            }

            PubSubRequest::Items {
                node,
                max_items,
                item_ids,
            } => {
                // Retrieve items from a node
                let result = self
                    .pubsub_storage
                    .get_items(&target_jid, &node, max_items, &item_ids)
                    .await;

                match result {
                    Ok(stored_items) => {
                        let items: Vec<PubSubItem> =
                            stored_items.iter().map(|si| si.to_pubsub_item()).collect();

                        debug!(
                            node = %node,
                            count = items.len(),
                            "PubSub items retrieved"
                        );

                        let response = build_pubsub_items_result(&iq, &node, &items);
                        self.stream.write_stanza(&Stanza::Iq(response)).await?;
                    }
                    Err(e) => {
                        warn!("PubSub items retrieval failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::NodeNotFound);
                        self.stream.write_stanza(&Stanza::Iq(error)).await?;
                    }
                }
            }

            PubSubRequest::Retract {
                node,
                item_id,
                notify: _,
            } => {
                // Only allow retracting from own nodes
                if target_jid != user_jid {
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    self.stream.write_stanza(&Stanza::Iq(error)).await?;
                    return Ok(());
                }

                let result = self
                    .pubsub_storage
                    .retract_item(&target_jid, &node, &item_id)
                    .await;

                match result {
                    Ok(retracted) => {
                        if retracted {
                            debug!(node = %node, item_id = %item_id, "PubSub item retracted");
                            let response = build_pubsub_success(&iq);
                            self.stream.write_stanza(&Stanza::Iq(response)).await?;
                        } else {
                            let error = build_pubsub_error(&iq, PubSubError::ItemNotFound);
                            self.stream.write_stanza(&Stanza::Iq(error)).await?;
                        }
                    }
                    Err(e) => {
                        warn!("PubSub retract failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::NodeNotFound);
                        self.stream.write_stanza(&Stanza::Iq(error)).await?;
                    }
                }
            }

            PubSubRequest::CreateNode { node } => {
                // Only allow creating own nodes
                if target_jid != user_jid {
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    self.stream.write_stanza(&Stanza::Iq(error)).await?;
                    return Ok(());
                }

                // For PEP, nodes are auto-created, but explicit create is also supported
                let result = self
                    .pubsub_storage
                    .get_or_create_node(&target_jid, &node)
                    .await;

                match result {
                    Ok((_, created)) => {
                        if created {
                            debug!(node = %node, "PubSub node created");
                        } else {
                            debug!(node = %node, "PubSub node already exists");
                        }
                        let response = build_pubsub_success(&iq);
                        self.stream.write_stanza(&Stanza::Iq(response)).await?;
                    }
                    Err(e) => {
                        warn!("PubSub node creation failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                        self.stream.write_stanza(&Stanza::Iq(error)).await?;
                    }
                }
            }

            PubSubRequest::DeleteNode { node } => {
                // Only allow deleting own nodes
                if target_jid != user_jid {
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    self.stream.write_stanza(&Stanza::Iq(error)).await?;
                    return Ok(());
                }

                let result = self.pubsub_storage.delete_node(&target_jid, &node).await;

                match result {
                    Ok(deleted) => {
                        if deleted {
                            debug!(node = %node, "PubSub node deleted");
                            let response = build_pubsub_success(&iq);
                            self.stream.write_stanza(&Stanza::Iq(response)).await?;
                        } else {
                            let error = build_pubsub_error(&iq, PubSubError::NodeNotFound);
                            self.stream.write_stanza(&Stanza::Iq(error)).await?;
                        }
                    }
                    Err(e) => {
                        warn!("PubSub node deletion failed: {}", e);
                        let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                        self.stream.write_stanza(&Stanza::Iq(error)).await?;
                    }
                }
            }

            PubSubRequest::Subscribe { .. } | PubSubRequest::Unsubscribe { .. } => {
                // PEP uses implicit presence-based subscriptions
                // For now, return success for compatibility
                debug!("PubSub subscribe/unsubscribe - using implicit PEP subscriptions");
                let response = build_pubsub_success(&iq);
                self.stream.write_stanza(&Stanza::Iq(response)).await?;
            }
        }

        Ok(())
    }

    /// Handle XEP-0352 Client State Indication: active.
    ///
    /// Called when the client sends `<active xmlns='urn:xmpp:csi:0'/>`.
    /// This typically indicates the client app is in the foreground.
    /// When transitioning from inactive to active, flushes any buffered stanzas.
    #[instrument(skip(self), name = "xmpp.csi.active")]
    async fn handle_csi_active(&mut self) -> Result<(), XmppError> {
        use crate::xep::xep0352::ClientState;

        let previous_state = self.client_state;
        self.client_state = ClientState::Active;

        if previous_state != ClientState::Active {
            debug!(
                jid = ?self.jid,
                buffered_stanzas = self.csi_buffer.len(),
                "Client state changed to Active, flushing buffer"
            );

            // Flush buffered stanzas to the client
            self.flush_csi_buffer().await?;
        }

        Ok(())
    }

    /// Flush all buffered CSI stanzas to the client.
    ///
    /// Called when the client transitions from inactive to active.
    async fn flush_csi_buffer(&mut self) -> Result<(), XmppError> {
        // Take the buffer to avoid borrowing issues
        let buffered = std::mem::take(&mut self.csi_buffer);

        if buffered.is_empty() {
            return Ok(());
        }

        debug!(count = buffered.len(), "Flushing CSI buffer");

        for stanza in buffered {
            self.stream.write_stanza(&stanza).await?;
        }

        Ok(())
    }

    /// Check if a stanza should be buffered when client is inactive.
    ///
    /// Returns true for non-critical stanzas that can be delayed:
    /// - Presence updates (except errors and subscription requests)
    /// - PubSub event notifications
    /// - Chat state notifications (messages without body)
    ///
    /// Returns false for critical stanzas that must be delivered immediately:
    /// - Direct messages with body content
    /// - MUC messages that mention the user's nickname
    /// - IQ requests/responses
    /// - Error stanzas
    /// - Subscription requests/responses
    fn should_buffer_stanza(&self, stanza: &Stanza) -> bool {
        use crate::xep::xep0352::{
            classify_message_urgency, classify_presence_urgency, is_muc_mention,
        };

        match stanza {
            // Use the centralized presence classification
            Stanza::Presence(pres) => classify_presence_urgency(pres).can_buffer(),
            // Messages need more careful consideration
            Stanza::Message(msg) => {
                // First check basic message urgency
                let urgency = classify_message_urgency(msg);
                if urgency.is_urgent() {
                    // Check if this is a MUC message that mentions the user
                    // Even urgent MUC messages without mentions could potentially be buffered,
                    // but MUC mentions should always be delivered immediately
                    if let Some(nickname) = self.get_user_muc_nickname() {
                        if is_muc_mention(msg, &nickname) {
                            return false; // Don't buffer - user was mentioned
                        }
                    }
                    // Urgent messages (with body) should not be buffered
                    return false;
                }
                // Non-urgent messages (chat states, receipts) can be buffered
                true
            }
            // IQs are request/response and should not be buffered
            Stanza::Iq(_) => false,
        }
    }

    /// Get the user's MUC nickname if available.
    ///
    /// This is used for MUC mention detection during CSI buffering.
    /// Returns the resource part of the JID as a fallback nickname.
    fn get_user_muc_nickname(&self) -> Option<String> {
        // For MUC mention detection, we use the resource part of the user's JID
        // as a reasonable default nickname. In a more sophisticated implementation,
        // we could track the actual nicknames used in each room.
        self.jid.as_ref().map(|jid| jid.resource().to_string())
    }

    /// Send a stanza to the client, respecting CSI state.
    ///
    /// If the client is inactive and the stanza is non-critical, it will be
    /// buffered for later delivery. Critical stanzas are sent immediately.
    ///
    /// Buffer behavior:
    /// - Non-urgent stanzas are buffered up to `MAX_CSI_BUFFER_SIZE` (100 stanzas)
    /// - When the buffer is full, new stanzas are sent immediately
    /// - When the client becomes active, buffered stanzas are flushed in order
    async fn send_stanza_with_csi(&mut self, stanza: Stanza) -> Result<(), XmppError> {
        use crate::xep::xep0352::{ClientState, MAX_CSI_BUFFER_SIZE};

        // If client is active, send immediately
        if self.client_state == ClientState::Active {
            return self.stream.write_stanza(&stanza).await;
        }

        // Client is inactive - check if we should buffer
        if self.should_buffer_stanza(&stanza) {
            // Buffer the stanza (with a reasonable limit to prevent memory issues)
            if self.csi_buffer.len() < MAX_CSI_BUFFER_SIZE {
                debug!(
                    stanza_type = ?std::mem::discriminant(&stanza),
                    buffer_size = self.csi_buffer.len(),
                    "Buffering stanza for inactive client"
                );
                self.csi_buffer.push(stanza);
                return Ok(());
            } else {
                debug!("CSI buffer full, sending stanza immediately");
            }
        }

        // Send critical stanzas immediately even when inactive
        self.stream.write_stanza(&stanza).await
    }

    /// Handle XEP-0352 Client State Indication: inactive.
    ///
    /// Called when the client sends `<inactive xmlns='urn:xmpp:csi:0'/>`.
    /// This typically indicates the client app is in the background.
    /// The server may use this to optimize traffic (batch stanzas, delay
    /// non-urgent presence updates, etc.).
    #[instrument(skip(self), name = "xmpp.csi.inactive")]
    async fn handle_csi_inactive(&mut self) -> Result<(), XmppError> {
        use crate::xep::xep0352::ClientState;

        let previous_state = self.client_state;
        self.client_state = ClientState::Inactive;

        if previous_state != ClientState::Inactive {
            debug!(
                jid = ?self.jid,
                "Client state changed to Inactive"
            );
        }

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
    async fn handle_isr_token_request(
        &mut self,
        iq: xmpp_parsers::iq::Iq,
    ) -> Result<(), XmppError> {
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
            self.isr_token_store
                .create_token(session.did.clone(), jid.clone())
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
            let carbon = build_sent_carbon(msg, &bare_jid.to_string(), &resource_jid.to_string());

            let stanza = Stanza::Message(carbon);
            let _ = self
                .connection_registry
                .send_to(&resource_jid, stanza)
                .await;
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
            let carbon =
                build_received_carbon(msg, &bare_jid.to_string(), &resource_jid.to_string());

            let stanza = Stanza::Message(carbon);
            let _ = self
                .connection_registry
                .send_to(&resource_jid, stanza)
                .await;
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
        let all_resources = self
            .connection_registry
            .get_resources_for_user(recipient_bare);

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
            let carbon =
                build_received_carbon(msg, &recipient_bare.to_string(), &resource_jid.to_string());

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
        let sender_bare = sender_jid.to_bare();

        // RFC 6121: roster retrieval only for the requester's own roster.
        if let Some(to) = &iq.to {
            let to_bare = to.to_bare();
            if to_bare != sender_bare {
                return Err(XmppError::not_authorized(Some(
                    "Cannot retrieve another user's roster".to_string(),
                )));
            }
            if to.resource().is_some() {
                return Err(XmppError::bad_request(Some(
                    "Roster get target MUST be a bare JID".to_string(),
                )));
            }
        }

        let items = self.app_state.get_roster(&sender_bare).await?;
        let roster_version = self.app_state.get_roster_version(&sender_bare).await?;

        // Build and send the roster result
        let response = build_roster_result(
            &iq,
            &items,
            roster_version.as_deref().or(query.ver.as_deref()),
        );
        self.stream.write_stanza(&Stanza::Iq(response)).await?;

        debug!(
            item_count = items.len(),
            ver = ?roster_version,
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

        let sender_bare = sender_jid.to_bare();

        // RFC 6121: roster updates only for the requester's own roster.
        if let Some(to) = &iq.to {
            let to_bare = to.to_bare();
            if to_bare != sender_bare {
                return Err(XmppError::forbidden(Some(
                    "Cannot update another user's roster".to_string(),
                )));
            }
            if to.resource().is_some() {
                return Err(XmppError::bad_request(Some(
                    "Roster set target MUST be a bare JID".to_string(),
                )));
            }
        }

        // Parse the roster set query
        let query = parse_roster_set(&iq)?;

        // Per RFC 6121, roster set MUST contain exactly one item.
        if query.items.len() != 1 {
            return Err(XmppError::bad_request(Some(
                "Roster set must contain exactly one item".to_string(),
            )));
        }
        let item = query.items.first().expect("checked len above");

        debug!(
            contact_jid = %item.jid,
            subscription = %item.subscription,
            is_remove = item.subscription.is_remove(),
            "Processing roster item"
        );

        let push_item = if item.subscription.is_remove() {
            let existing_item = self
                .app_state
                .get_roster_item(&sender_bare, &item.jid)
                .await?;

            match existing_item {
                Some(existing_item) => {
                    let _ = self
                        .app_state
                        .remove_roster_item(&sender_bare, &item.jid)
                        .await?;

                    self.handle_roster_remove_side_effects(&sender_jid, &sender_bare, &existing_item)
                        .await?;

                    RosterItem::new(item.jid.clone()).set_subscription(Subscription::Remove)
                }
                None if item.name.is_some() || !item.groups.is_empty() => {
                    // Some clients resend remove requests with legacy metadata (name/groups).
                    // Treat these as idempotent no-op removals.
                    RosterItem::new(item.jid.clone()).set_subscription(Subscription::Remove)
                }
                None => {
                    return Err(XmppError::item_not_found(Some(format!(
                        "Roster item {} not found",
                        item.jid
                    ))));
                }
            }
        } else {
            let existing = self
                .app_state
                .get_roster_item(&sender_bare, &item.jid)
                .await?;

            // Client-provided subscription/ask values are ignored for roster set updates.
            let mut effective_item = item.clone();
            if let Some(existing_item) = existing {
                effective_item.subscription = existing_item.subscription;
                effective_item.ask = existing_item.ask;
            } else {
                effective_item.subscription = Subscription::None;
                effective_item.ask = None;
            }

            let _ = self
                .app_state
                .set_roster_item(&sender_bare, &effective_item)
                .await?;
            effective_item
        };

        // Send empty result to acknowledge the roster set
        let response = build_roster_result_empty(&iq);
        self.stream.write_stanza(&Stanza::Iq(response)).await?;

        // RFC 6121: Send roster push to all connected resources
        // This notifies all user's clients about the roster change
        self.send_roster_push(&push_item).await;

        info!(
            contact = %push_item.jid,
            subscription = %push_item.subscription,
            "Roster set processed"
        );

        Ok(())
    }

    /// Apply RFC 6121 side effects when removing a roster item.
    ///
    /// Depending on the previous subscription state, this may emit
    /// `<presence type='unsubscribe'/>` and/or `<presence type='unsubscribed'/>`
    /// and update the contact-side roster state.
    async fn handle_roster_remove_side_effects(
        &self,
        sender_full: &FullJid,
        sender_bare: &jid::BareJid,
        removed_item: &RosterItem,
    ) -> Result<(), XmppError> {
        let contact_jid = removed_item.jid.clone();
        let send_unsubscribe = matches!(removed_item.subscription, Subscription::To | Subscription::Both);
        let send_unsubscribed = matches!(
            removed_item.subscription,
            Subscription::From | Subscription::Both
        );

        if send_unsubscribe {
            let mut unsubscribe = xmpp_parsers::presence::Presence::new(PresenceType::Unsubscribe);
            unsubscribe.from = Some(sender_full.clone().into());
            unsubscribe.to = Some(contact_jid.clone().into());
            self.route_stanza_to_bare_jid(&contact_jid, Stanza::Presence(unsubscribe))
                .await?;
        }

        if send_unsubscribed {
            let mut unsubscribed =
                xmpp_parsers::presence::Presence::new(PresenceType::Unsubscribed);
            unsubscribed.from = Some(sender_full.clone().into());
            unsubscribed.to = Some(contact_jid.clone().into());
            self.route_stanza_to_bare_jid(&contact_jid, Stanza::Presence(unsubscribed))
                .await?;
        }

        // Keep contact-side roster state in sync with the generated subscription stanzas.
        if send_unsubscribe || send_unsubscribed {
            if let Some(mut contact_item) = self
                .app_state
                .get_roster_item(&contact_jid, sender_bare)
                .await?
            {
                let before = contact_item.clone();

                // Contact receives an inbound <unsubscribe/> from sender.
                if send_unsubscribe {
                    SubscriptionStateMachine::apply_outbound_unsubscribed(&mut contact_item);
                }

                // Contact receives an inbound <unsubscribed/> from sender.
                if send_unsubscribed {
                    SubscriptionStateMachine::apply_outbound_unsubscribe(&mut contact_item);
                }

                if contact_item != before {
                    let _ = self
                        .app_state
                        .set_roster_item(&contact_jid, &contact_item)
                        .await?;
                    self.send_roster_push_for_user(&contact_jid, &contact_item)
                        .await;
                }
            }
        }

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
        self.send_roster_push_for_user(&sender_jid.to_bare(), item)
            .await;
    }

    /// Send roster push to all connected resources for an arbitrary bare JID.
    async fn send_roster_push_for_user(&self, bare_jid: &jid::BareJid, item: &RosterItem) {
        // Get all connected resources for this user (including self)
        let resources = self.connection_registry.get_resources_for_user(bare_jid);

        if resources.is_empty() {
            return;
        }

        let roster_version = match self.app_state.get_roster_version(bare_jid).await {
            Ok(ver) => ver,
            Err(error) => {
                warn!(user = %bare_jid, error = %error, "Failed to fetch roster version for push");
                None
            }
        };

        debug!(
            resource_count = resources.len(),
            contact = %item.jid,
            ver = ?roster_version,
            "Sending roster push to connected resources"
        );

        for resource_jid in resources {
            // Generate a unique push ID
            let push_id = format!("push-{}", uuid::Uuid::new_v4());

            let push = build_roster_push(
                &push_id,
                &resource_jid.to_string(),
                item,
                roster_version.as_deref(),
            );

            let stanza = Stanza::Iq(push);
            let result = self
                .connection_registry
                .send_to(&resource_jid, stanza)
                .await;

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
                        let vcard =
                            crate::xep::xep0054::parse_vcard_element(&elem).unwrap_or_default();
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
        let vcard = parse_vcard_from_iq(&iq)
            .map_err(|e| XmppError::bad_request(Some(format!("Invalid vCard: {}", e))))?;

        // Build the vCard XML for storage
        let vcard_elem = crate::xep::xep0054::build_vcard_element(&vcard);
        let vcard_xml = String::from(&vcard_elem);

        // Store the vCard
        self.app_state
            .set_vcard(&sender_jid.to_bare(), &vcard_xml)
            .await?;

        // XEP-0153: Compute avatar hash from vCard PHOTO if present
        if let Some(ref photo) = vcard.photo {
            let hash = crate::xep::xep0153::compute_photo_hash_from_base64(&photo.data);
            self.avatar_hash = hash;

            // XEP-0398: Convert vCard PHOTO to PEP avatar (if not already converting)
            if !self.converting_avatar {
                self.converting_avatar = true;
                let converter = crate::xep::xep0398::DefaultAvatarConversion;
                if let Some((avatar_info, avatar_data_b64)) =
                    converter.on_vcard_photo_updated(&photo.data, &photo.mime_type)
                {
                    // Publish avatar data to PEP
                    let data_elem = crate::xep::xep0084::build_avatar_data(&avatar_data_b64);
                    let data_item = PubSubItem::new(Some(avatar_info.id.clone()), Some(data_elem));
                    let _ = self
                        .pubsub_storage
                        .publish_item(
                            &sender_jid.to_bare(),
                            crate::xep::xep0084::NODE_AVATAR_DATA,
                            &data_item,
                            Some(&sender_jid.to_bare()),
                            true,
                        )
                        .await;

                    // Publish avatar metadata to PEP
                    let metadata_elem = crate::xep::xep0084::build_avatar_metadata(&avatar_info);
                    let metadata_item =
                        PubSubItem::new(Some(avatar_info.id.clone()), Some(metadata_elem));
                    let _ = self
                        .pubsub_storage
                        .publish_item(
                            &sender_jid.to_bare(),
                            crate::xep::xep0084::NODE_AVATAR_METADATA,
                            &metadata_item,
                            Some(&sender_jid.to_bare()),
                            true,
                        )
                        .await;
                }
                self.converting_avatar = false;
            }
        } else {
            // No photo in vCard, clear the hash
            self.avatar_hash = None;
        }

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
    async fn handle_upload_slot_request(
        &mut self,
        iq: xmpp_parsers::iq::Iq,
    ) -> Result<(), XmppError> {
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
            let error_response =
                build_upload_error(&iq.id, &UploadError::FileTooLarge { max_size });
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
                let error_response =
                    build_upload_error(&iq.id, &UploadError::InternalError(e.to_string()));
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

    /// Handle XEP-0045 MUC admin IQ requests.
    ///
    /// Processes admin operations for MUC rooms:
    /// - GET: Query affiliation/role lists (members, admins, owners, outcasts, moderators)
    /// - SET: Modify affiliations (grant member/admin/owner, ban users)
    /// - SET: Modify roles (grant moderator/participant/visitor, kick users)
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_muc_admin_iq(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => {
                warn!("MUC admin IQ received before JID bound");
                return Err(XmppError::not_authorized(Some(
                    "Session not established".to_string(),
                )));
            }
        };

        let muc_domain = self.room_registry.muc_domain();
        let query = match parse_admin_query(&iq, muc_domain) {
            Ok(q) => q,
            Err(e) => {
                warn!(error = %e, "Failed to parse MUC admin query");
                let error = crate::generate_iq_error(
                    &iq.id,
                    iq.to.as_ref().map(|j| j.to_string()).as_deref(),
                    iq.from.as_ref().map(|j| j.to_string()).as_deref(),
                    crate::StanzaErrorCondition::BadRequest,
                    crate::StanzaErrorType::Cancel,
                    Some(&e.to_string()),
                );
                self.stream.write_raw(&error).await?;
                return Ok(());
            }
        };

        debug!(
            room = %query.room_jid,
            is_get = query.is_get,
            item_count = query.items.len(),
            "Processing MUC admin IQ"
        );

        // Get the room
        let room_data = match self.room_registry.get_room_data(&query.room_jid) {
            Some(data) => data,
            None => {
                let error = crate::generate_iq_error(
                    &iq.id,
                    iq.to.as_ref().map(|j| j.to_string()).as_deref(),
                    iq.from.as_ref().map(|j| j.to_string()).as_deref(),
                    crate::StanzaErrorCondition::ItemNotFound,
                    crate::StanzaErrorType::Cancel,
                    Some(&format!("Room {} not found", query.room_jid)),
                );
                self.stream.write_raw(&error).await?;
                return Ok(());
            }
        };

        if query.is_get {
            // Handle GET - query affiliation or role list
            let room = room_data.read().await;

            // Check if sender has permission to view affiliations/roles
            // Per XEP-0045, admins and owners can query affiliation lists
            // Moderators can query role lists
            let sender_affiliation = room.get_affiliation(&sender_jid.to_bare());
            let sender_occupant = room.find_occupant_by_real_jid(&sender_jid);
            let sender_role = sender_occupant.map(|o| o.role).unwrap_or(Role::None);

            // Check if this is a role query (has role attribute) or affiliation query
            let requested_role = query.items.first().and_then(|item| item.role);

            if requested_role.is_some() {
                // Role query - moderators and above can query
                if sender_role < Role::Moderator
                    && !matches!(sender_affiliation, Affiliation::Owner | Affiliation::Admin)
                {
                    let error = crate::generate_iq_error(
                        &iq.id,
                        iq.to.as_ref().map(|j| j.to_string()).as_deref(),
                        iq.from.as_ref().map(|j| j.to_string()).as_deref(),
                        crate::StanzaErrorCondition::Forbidden,
                        crate::StanzaErrorType::Auth,
                        Some("Only moderators can query role lists"),
                    );
                    self.stream.write_raw(&error).await?;
                    return Ok(());
                }

                // Build role list response
                let role = requested_role.unwrap();
                let items: Vec<(String, Role, Option<jid::BareJid>)> = room
                    .occupants
                    .values()
                    .filter(|o| o.role == role)
                    .map(|o| (o.nick.clone(), o.role, Some(o.real_jid.to_bare())))
                    .collect();

                let response =
                    build_role_result(&query.iq_id, &query.room_jid, &query.from, &items);
                self.stream.write_stanza(&Stanza::Iq(response)).await?;

                debug!(
                    room = %query.room_jid,
                    item_count = items.len(),
                    role = ?role,
                    "Sent MUC role query result"
                );
            } else {
                // Affiliation query - admins and owners can query
                if !matches!(sender_affiliation, Affiliation::Owner | Affiliation::Admin) {
                    let error = crate::generate_iq_error(
                        &iq.id,
                        iq.to.as_ref().map(|j| j.to_string()).as_deref(),
                        iq.from.as_ref().map(|j| j.to_string()).as_deref(),
                        crate::StanzaErrorCondition::Forbidden,
                        crate::StanzaErrorType::Auth,
                        Some("Only admins and owners can query affiliation lists"),
                    );
                    self.stream.write_raw(&error).await?;
                    return Ok(());
                }

                // Get the requested affiliation from the first item (per XEP-0045)
                let requested_affiliation = query.items.first().and_then(|item| item.affiliation);

                let items: Vec<(jid::BareJid, Affiliation)> = match requested_affiliation {
                    Some(aff) => room
                        .get_jids_by_affiliation(aff)
                        .into_iter()
                        .map(|jid| (jid, aff))
                        .collect(),
                    None => room
                        .get_all_affiliations()
                        .into_iter()
                        .map(|entry| (entry.jid, entry.affiliation))
                        .collect(),
                };

                let response =
                    build_admin_result(&query.iq_id, &query.room_jid, &query.from, &items);
                self.stream.write_stanza(&Stanza::Iq(response)).await?;

                debug!(
                    room = %query.room_jid,
                    item_count = items.len(),
                    "Sent MUC admin query result"
                );
            }
        } else {
            // Handle SET - modify affiliations or roles
            let mut room = room_data.write().await;

            // Determine if this is a role change or affiliation change
            let is_role_change = is_role_change_query(&query.items);

            // Check permissions based on operation type
            let sender_affiliation = room.get_affiliation(&sender_jid.to_bare());
            let sender_occupant = room.find_occupant_by_real_jid(&sender_jid);
            let sender_role = sender_occupant.map(|o| o.role).unwrap_or(Role::None);

            // Collect presence updates to broadcast after processing
            let mut presence_updates: Vec<(jid::FullJid, xmpp_parsers::presence::Presence)> =
                Vec::new();
            // Collect occupants to remove (for kicks and bans)
            let mut occupants_to_kick: Vec<String> = Vec::new();

            if is_role_change {
                // Handle role changes (kicks, granting/revoking voice)
                for item in &query.items {
                    let target_nick = match &item.nick {
                        Some(nick) => nick.clone(),
                        None => continue, // Skip items without nick
                    };

                    let new_role = match item.role {
                        Some(role) => role,
                        None => continue, // Skip items without role
                    };

                    // Find the target occupant
                    let target_occupant = match room.get_occupant(&target_nick) {
                        Some(occ) => occ.clone(),
                        None => {
                            let error = crate::generate_iq_error(
                                &iq.id,
                                iq.to.as_ref().map(|j| j.to_string()).as_deref(),
                                iq.from.as_ref().map(|j| j.to_string()).as_deref(),
                                crate::StanzaErrorCondition::ItemNotFound,
                                crate::StanzaErrorType::Cancel,
                                Some(&format!("Occupant '{}' not found in room", target_nick)),
                            );
                            self.stream.write_raw(&error).await?;
                            return Ok(());
                        }
                    };

                    // Permission check for role changes
                    // Per XEP-0045 §8.2-8.5:
                    // - Moderators can change roles of participants and visitors
                    // - Admins can change roles of any non-owner
                    // - Owners can change roles of anyone
                    let can_modify = match (
                        sender_affiliation,
                        sender_role,
                        target_occupant.affiliation,
                        new_role,
                    ) {
                        // Owners can do anything
                        (Affiliation::Owner, _, _, _) => true,
                        // Admins can modify non-owners
                        (Affiliation::Admin, _, target_aff, _)
                            if target_aff != Affiliation::Owner =>
                        {
                            true
                        }
                        // Moderators can modify participants and visitors
                        (_, Role::Moderator, target_aff, _)
                            if !matches!(target_aff, Affiliation::Owner | Affiliation::Admin) =>
                        {
                            true
                        }
                        _ => false,
                    };

                    if !can_modify {
                        let error = crate::generate_iq_error(
                            &iq.id,
                            iq.to.as_ref().map(|j| j.to_string()).as_deref(),
                            iq.from.as_ref().map(|j| j.to_string()).as_deref(),
                            crate::StanzaErrorCondition::NotAllowed,
                            crate::StanzaErrorType::Cancel,
                            Some("You don't have permission to change this user's role"),
                        );
                        self.stream.write_raw(&error).await?;
                        return Ok(());
                    }

                    // Build the room JID with the target's nick
                    let from_room_jid = match query.room_jid.with_resource_str(&target_nick) {
                        Ok(jid) => jid,
                        Err(_) => continue,
                    };

                    if new_role == Role::None {
                        // This is a kick operation
                        debug!(
                            room = %query.room_jid,
                            target = %target_nick,
                            actor = %sender_jid.to_bare(),
                            reason = ?item.reason,
                            "Kicking occupant"
                        );

                        // Build kick presence for all occupants
                        for (nick, occupant) in room.occupants.iter() {
                            let is_self = nick == &target_nick;
                            let presence = build_kick_presence(
                                &from_room_jid,
                                &occupant.real_jid,
                                target_occupant.affiliation,
                                is_self,
                                item.reason.as_deref(),
                                Some(&sender_jid.to_bare()),
                            );
                            presence_updates.push((occupant.real_jid.clone(), presence));
                        }

                        occupants_to_kick.push(target_nick);
                    } else {
                        // Role change (voice grant/revoke, moderator grant/revoke)
                        debug!(
                            room = %query.room_jid,
                            target = %target_nick,
                            old_role = ?target_occupant.role,
                            new_role = ?new_role,
                            "Changing occupant role"
                        );

                        // Update occupant's role
                        if let Some(occ) = room.occupants.get_mut(&target_nick) {
                            occ.role = new_role;
                        }

                        // Build role change presence for all occupants
                        for (nick, occupant) in room.occupants.iter() {
                            let is_self = nick == &target_nick;
                            let presence = build_role_change_presence(
                                &from_room_jid,
                                &occupant.real_jid,
                                target_occupant.affiliation,
                                new_role,
                                is_self,
                                None,
                            );
                            presence_updates.push((occupant.real_jid.clone(), presence));
                        }
                    }
                }
            } else {
                // Handle affiliation changes
                for item in &query.items {
                    let target_jid = match &item.jid {
                        Some(jid) => jid.clone(),
                        None => continue, // Skip items without JID
                    };

                    let new_affiliation = match item.affiliation {
                        Some(aff) => aff,
                        None => continue, // Skip items without affiliation
                    };

                    // Permission check: who can set what affiliation
                    // Per XEP-0045 §10.6:
                    // - Only owners can grant/revoke owner status
                    // - Owners and admins can grant/revoke admin/member status
                    // - Owners and admins can ban (outcast) users
                    let can_modify = match new_affiliation {
                        Affiliation::Owner => sender_affiliation == Affiliation::Owner,
                        Affiliation::Admin
                        | Affiliation::Member
                        | Affiliation::None
                        | Affiliation::Outcast => {
                            matches!(sender_affiliation, Affiliation::Owner | Affiliation::Admin)
                        }
                    };

                    if !can_modify {
                        let error = crate::generate_iq_error(
                            &iq.id,
                            iq.to.as_ref().map(|j| j.to_string()).as_deref(),
                            iq.from.as_ref().map(|j| j.to_string()).as_deref(),
                            crate::StanzaErrorCondition::Forbidden,
                            crate::StanzaErrorType::Auth,
                            Some(&format!(
                                "You don't have permission to set {} affiliation",
                                crate::muc::admin::affiliation_to_str(new_affiliation)
                            )),
                        );
                        self.stream.write_raw(&error).await?;
                        return Ok(());
                    }

                    // Cannot demote the last owner
                    if new_affiliation != Affiliation::Owner {
                        let target_current_affiliation = room.get_affiliation(&target_jid);
                        if target_current_affiliation == Affiliation::Owner {
                            let owners = room.get_jids_by_affiliation(Affiliation::Owner);
                            if owners.len() == 1 && owners.contains(&target_jid) {
                                let error = crate::generate_iq_error(
                                    &iq.id,
                                    iq.to.as_ref().map(|j| j.to_string()).as_deref(),
                                    iq.from.as_ref().map(|j| j.to_string()).as_deref(),
                                    crate::StanzaErrorCondition::Conflict,
                                    crate::StanzaErrorType::Cancel,
                                    Some("Cannot remove the last owner from a room"),
                                );
                                self.stream.write_raw(&error).await?;
                                return Ok(());
                            }
                        }
                    }

                    // Set the affiliation
                    let change = room.set_affiliation(target_jid.clone(), new_affiliation);

                    if let Some(change) = change {
                        debug!(
                            room = %query.room_jid,
                            target = %target_jid,
                            old_affiliation = ?change.old_affiliation,
                            new_affiliation = ?change.new_affiliation,
                            "Affiliation changed"
                        );

                        // Find occupant with this JID if they are in the room
                        let affected_occupant = room
                            .occupants
                            .values()
                            .find(|o| o.real_jid.to_bare() == target_jid)
                            .cloned();

                        if let Some(occupant) = affected_occupant {
                            let from_room_jid =
                                match query.room_jid.with_resource_str(&occupant.nick) {
                                    Ok(jid) => jid,
                                    Err(_) => continue,
                                };

                            if new_affiliation == Affiliation::Outcast {
                                // Ban - kick the user and send ban presence
                                debug!(
                                    room = %query.room_jid,
                                    target = %occupant.nick,
                                    actor = %sender_jid.to_bare(),
                                    reason = ?item.reason,
                                    "Banning occupant"
                                );

                                // Build ban presence for all occupants
                                for (nick, occ) in room.occupants.iter() {
                                    let is_self = nick == &occupant.nick;
                                    let presence = build_ban_presence(
                                        &from_room_jid,
                                        &occ.real_jid,
                                        is_self,
                                        item.reason.as_deref(),
                                        Some(&sender_jid.to_bare()),
                                    );
                                    presence_updates.push((occ.real_jid.clone(), presence));
                                }

                                occupants_to_kick.push(occupant.nick.clone());
                            } else {
                                // Regular affiliation change - send presence update
                                for (nick, occ) in room.occupants.iter() {
                                    let is_self = nick == &occupant.nick;
                                    let presence = build_affiliation_change_presence(
                                        &from_room_jid,
                                        &occ.real_jid,
                                        new_affiliation,
                                        occupant.role,
                                        is_self,
                                        None,
                                    );
                                    presence_updates.push((occ.real_jid.clone(), presence));
                                }
                            }
                        }
                    }
                }
            }

            // Remove kicked/banned occupants from the room
            for nick in occupants_to_kick {
                room.remove_occupant(&nick);
            }

            // Drop the lock before sending presence updates
            drop(room);

            // Send all presence updates
            for (to_jid, presence) in presence_updates {
                match self
                    .connection_registry
                    .send_to(&to_jid, Stanza::Presence(presence))
                    .await
                {
                    SendResult::Sent => {
                        debug!(to = %to_jid, "Sent admin presence update");
                    }
                    _ => {
                        debug!(to = %to_jid, "Failed to send admin presence update (user may be offline)");
                    }
                }
            }

            // Send success response
            let response = build_admin_set_result(&query.iq_id, &query.room_jid, &query.from);
            self.stream.write_stanza(&Stanza::Iq(response)).await?;

            debug!(
                room = %query.room_jid,
                "MUC admin set completed"
            );
        }

        Ok(())
    }

    /// Handle a MUC owner IQ (XEP-0045 §10.1-10.2).
    ///
    /// Processes owner operations for MUC rooms:
    /// - GET: Retrieve room configuration form
    /// - SET with form: Update room configuration
    /// - SET with destroy: Destroy the room
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_muc_owner_iq(&mut self, mut iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => {
                warn!("MUC owner IQ received before JID bound");
                return Err(XmppError::not_authorized(Some(
                    "Session not established".to_string(),
                )));
            }
        };

        // Client stanzas often omit 'from'; normalize it for owner query parsing.
        if iq.from.is_none() {
            iq.from = Some(sender_jid.clone().into());
        }

        let muc_domain = self.room_registry.muc_domain();
        let query = match parse_owner_query(&iq, muc_domain) {
            Ok(q) => q,
            Err(e) => {
                warn!(error = %e, "Failed to parse MUC owner query");
                let error = crate::generate_iq_error(
                    &iq.id,
                    iq.to.as_ref().map(|j| j.to_string()).as_deref(),
                    iq.from.as_ref().map(|j| j.to_string()).as_deref(),
                    crate::StanzaErrorCondition::BadRequest,
                    crate::StanzaErrorType::Cancel,
                    Some(&e.to_string()),
                );
                self.stream.write_raw(&error).await?;
                return Ok(());
            }
        };

        debug!(
            room = %query.room_jid,
            action = ?query.action,
            "Processing MUC owner IQ"
        );

        // Get the room
        let room_data = match self.room_registry.get_room_data(&query.room_jid) {
            Some(data) => data,
            None => {
                let error = crate::generate_iq_error(
                    &iq.id,
                    iq.to.as_ref().map(|j| j.to_string()).as_deref(),
                    iq.from.as_ref().map(|j| j.to_string()).as_deref(),
                    crate::StanzaErrorCondition::ItemNotFound,
                    crate::StanzaErrorType::Cancel,
                    Some(&format!("Room {} not found", query.room_jid)),
                );
                self.stream.write_raw(&error).await?;
                return Ok(());
            }
        };

        // Check if sender is a room owner
        {
            let room = room_data.read().await;
            let sender_affiliation = room.get_affiliation(&sender_jid.to_bare());
            if sender_affiliation != Affiliation::Owner {
                let error = crate::generate_iq_error(
                    &iq.id,
                    iq.to.as_ref().map(|j| j.to_string()).as_deref(),
                    iq.from.as_ref().map(|j| j.to_string()).as_deref(),
                    crate::StanzaErrorCondition::Forbidden,
                    crate::StanzaErrorType::Auth,
                    Some("Only room owners can perform owner operations"),
                );
                self.stream.write_raw(&error).await?;
                return Ok(());
            }
        }

        match query.action {
            OwnerAction::GetConfig => {
                // Return room configuration form
                let room = room_data.read().await;
                let config_form = build_config_form(&room);
                let response =
                    build_config_result(&query.iq_id, &query.room_jid, &query.from, config_form);
                self.stream.write_stanza(&Stanza::Iq(response)).await?;

                debug!(
                    room = %query.room_jid,
                    "Sent room configuration form"
                );
            }

            OwnerAction::SetConfig(form_data) => {
                // Update room configuration
                {
                    let mut room = room_data.write().await;
                    apply_config_form(&mut room.config, &form_data);

                    debug!(
                        room = %query.room_jid,
                        name = ?form_data.name,
                        persistent = ?form_data.persistent,
                        members_only = ?form_data.members_only,
                        "Room configuration updated"
                    );
                }

                // Send success response
                let response = build_owner_set_result(&query.iq_id, &query.room_jid, &query.from);
                self.stream.write_stanza(&Stanza::Iq(response)).await?;

                debug!(
                    room = %query.room_jid,
                    "Room configuration set completed"
                );
            }

            OwnerAction::Destroy(destroy_request) => {
                // Destroy the room
                let mut presence_updates: Vec<(jid::FullJid, xmpp_parsers::presence::Presence)> =
                    Vec::new();

                {
                    let room = room_data.read().await;

                    debug!(
                        room = %query.room_jid,
                        occupant_count = room.occupants.len(),
                        reason = ?destroy_request.reason,
                        alternate = ?destroy_request.alternate_venue,
                        "Destroying room"
                    );

                    // Build destroy notifications for all occupants
                    for (nick, occupant) in room.occupants.iter() {
                        let is_self = occupant.real_jid == sender_jid;
                        let presence = build_destroy_notification(
                            &query.room_jid,
                            nick,
                            &occupant.real_jid,
                            &destroy_request,
                            is_self,
                        );
                        presence_updates.push((occupant.real_jid.clone(), presence));
                    }
                }

                // Send destroy notifications to all occupants
                for (to_jid, presence) in presence_updates {
                    match self
                        .connection_registry
                        .send_to(&to_jid, Stanza::Presence(presence))
                        .await
                    {
                        SendResult::Sent => {
                            debug!(to = %to_jid, "Sent room destroy notification");
                        }
                        _ => {
                            debug!(to = %to_jid, "Failed to send room destroy notification (user may be offline)");
                        }
                    }
                }

                // Remove the room from the registry
                self.room_registry.destroy_room(&query.room_jid);

                // Send success response
                let response = build_owner_set_result(&query.iq_id, &query.room_jid, &query.from);
                self.stream.write_stanza(&Stanza::Iq(response)).await?;

                info!(
                    room = %query.room_jid,
                    reason = ?destroy_request.reason,
                    "Room destroyed"
                );
            }
        }

        Ok(())
    }

    /// Handle XEP-0049 Private XML Storage IQ requests.
    ///
    /// Processes private storage operations:
    /// - GET: Returns stored private XML for the given namespace
    /// - SET: Stores private XML data keyed by namespace
    ///
    /// Also intercepts `storage:bookmarks` namespace queries and delegates
    /// to XEP-0048 legacy bookmark compatibility.
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_private_storage(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => {
                warn!("Private storage query received before JID bound");
                return Err(XmppError::not_authorized(Some(
                    "Session not established".to_string(),
                )));
            }
        };

        let bare_jid = sender_jid.to_bare();

        // Handle GET requests
        if let Some(key) = parse_private_storage_get(&iq) {
            debug!(
                jid = %bare_jid,
                namespace = %key.namespace,
                "Processing private storage GET"
            );

            // Check if this is a legacy bookmarks query (XEP-0048)
            if crate::xep::xep0048::is_legacy_bookmarks_namespace(&key.namespace) {
                // Delegate to XEP-0048 compat: fetch from PEP bookmarks and convert
                let stored_items = self
                    .pubsub_storage
                    .get_items(&bare_jid, crate::xep::xep0402::PEP_NODE, None, &[])
                    .await
                    .unwrap_or_default();

                let native_bookmarks: Vec<crate::xep::xep0402::Bookmark> = stored_items
                    .iter()
                    .filter_map(|item| {
                        let xml = item.payload_xml.as_ref()?;
                        let elem: minidom::Element = xml.parse().ok()?;
                        crate::xep::xep0402::parse_bookmark(&item.id, &elem).ok()
                    })
                    .collect();

                let legacy: Vec<crate::xep::xep0048::LegacyBookmark> = native_bookmarks
                    .iter()
                    .map(crate::xep::xep0048::from_native_bookmark)
                    .collect();

                let bookmarks_elem = crate::xep::xep0048::build_legacy_bookmarks_element(&legacy);
                let bookmarks_xml = String::from(&bookmarks_elem);
                let response = build_private_storage_result(&iq, Some(&bookmarks_xml), &key);
                self.stream.write_stanza(&Stanza::Iq(response)).await?;
                return Ok(());
            }

            // Regular private XML storage
            let stored = self
                .app_state
                .get_private_xml(&bare_jid, &key.namespace)
                .await?;

            let response = build_private_storage_result(&iq, stored.as_deref(), &key);
            self.stream.write_stanza(&Stanza::Iq(response)).await?;
            return Ok(());
        }

        // Handle SET requests
        if let Some((key, xml_content)) = parse_private_storage_set(&iq) {
            debug!(
                jid = %bare_jid,
                namespace = %key.namespace,
                "Processing private storage SET"
            );

            // Check if this is a legacy bookmarks set (XEP-0048)
            if crate::xep::xep0048::is_legacy_bookmarks_namespace(&key.namespace) {
                // Parse legacy bookmarks and convert to native (XEP-0402)
                if let Ok(elem) = xml_content.parse::<minidom::Element>() {
                    let legacy = crate::xep::xep0048::parse_legacy_bookmarks(&elem);
                    for lb in &legacy {
                        if let Some(native) = crate::xep::xep0048::to_native_bookmark(lb) {
                            let bookmark_elem =
                                crate::xep::xep0402::build_bookmark_element(&native);
                            let item =
                                PubSubItem::new(Some(native.jid.to_string()), Some(bookmark_elem));
                            let _ = self
                                .pubsub_storage
                                .publish_item(
                                    &bare_jid,
                                    crate::xep::xep0402::PEP_NODE,
                                    &item,
                                    Some(&bare_jid),
                                    true,
                                )
                                .await;
                        }
                    }
                }

                let response = build_private_storage_success(&iq);
                self.stream.write_stanza(&Stanza::Iq(response)).await?;
                return Ok(());
            }

            // Regular private XML storage
            self.app_state
                .set_private_xml(&bare_jid, &key.namespace, &xml_content)
                .await?;

            let response = build_private_storage_success(&iq);
            self.stream.write_stanza(&Stanza::Iq(response)).await?;
            return Ok(());
        }

        // Neither GET nor SET parsed successfully
        let error = crate::generate_iq_error(
            &iq.id,
            iq.from.as_ref().map(|j| j.to_string()).as_deref(),
            Some(&self.domain),
            crate::StanzaErrorCondition::BadRequest,
            crate::StanzaErrorType::Modify,
            None,
        );
        self.stream.write_raw(&error).await?;
        Ok(())
    }

    /// Handle XEP-0191 Blocking Command IQ requests.
    ///
    /// Processes blocking operations:
    /// - GET blocklist: Returns the user's blocked JID list
    /// - SET block: Adds JIDs to the blocklist
    /// - SET unblock: Removes JIDs from the blocklist (empty = unblock all)
    #[instrument(skip(self, iq), fields(iq_id = %iq.id))]
    async fn handle_blocking_query(&mut self, iq: xmpp_parsers::iq::Iq) -> Result<(), XmppError> {
        let sender_jid = match &self.jid {
            Some(jid) => jid.clone(),
            None => {
                warn!("Blocking query received before JID bound");
                return Err(XmppError::not_authorized(Some(
                    "Session not established".to_string(),
                )));
            }
        };

        debug!(
            from = %sender_jid,
            "Processing blocking query"
        );

        // Parse the blocking request
        let request = match parse_blocking_request(&iq) {
            Ok(req) => req,
            Err(e) => {
                warn!(error = %e, "Failed to parse blocking request");
                let error_response = build_blocking_error(&iq.id, &e);
                self.stream.write_raw(&error_response).await?;
                return Ok(());
            }
        };

        let user_bare_jid = sender_jid.to_bare();

        match request {
            BlockingRequest::GetBlocklist => {
                // Retrieve the user's blocklist
                let blocked_jids = self.app_state.get_blocklist(&user_bare_jid).await?;

                debug!(
                    from = %sender_jid,
                    count = blocked_jids.len(),
                    "Returning blocklist"
                );

                let response = build_blocklist_response(&iq, &blocked_jids);
                self.stream.write_stanza(&Stanza::Iq(response)).await?;
            }

            BlockingRequest::Block(jids_to_block) => {
                // Add JIDs to the blocklist
                let added = self
                    .app_state
                    .add_blocks(&user_bare_jid, &jids_to_block)
                    .await?;

                debug!(
                    from = %sender_jid,
                    requested = jids_to_block.len(),
                    added = added,
                    "Added JIDs to blocklist"
                );

                // Send success response
                let response = build_blocking_success(&iq);
                self.stream.write_stanza(&Stanza::Iq(response)).await?;

                // Send push notifications to all user's connected resources
                self.send_block_push(&jids_to_block).await;
            }

            BlockingRequest::Unblock(jids_to_unblock) => {
                let (removed, unblocked_jids) = if jids_to_unblock.is_empty() {
                    // Unblock all - first get the current blocklist for push notification
                    let current_blocklist = self.app_state.get_blocklist(&user_bare_jid).await?;
                    let removed = self.app_state.remove_all_blocks(&user_bare_jid).await?;
                    (removed, current_blocklist)
                } else {
                    // Unblock specific JIDs
                    let removed = self
                        .app_state
                        .remove_blocks(&user_bare_jid, &jids_to_unblock)
                        .await?;
                    (removed, jids_to_unblock)
                };

                debug!(
                    from = %sender_jid,
                    requested = unblocked_jids.len(),
                    removed = removed,
                    "Removed JIDs from blocklist"
                );

                // Send success response
                let response = build_blocking_success(&iq);
                self.stream.write_stanza(&Stanza::Iq(response)).await?;

                // Send push notifications to all user's connected resources
                self.send_unblock_push(&unblocked_jids).await;
            }
        }

        Ok(())
    }

    /// Send block push notifications to all of the user's connected resources.
    ///
    /// Per XEP-0191, when a client blocks a JID, the server MUST send
    /// a push notification to all of the user's connected resources.
    async fn send_block_push(&self, blocked_jids: &[String]) {
        let sender_jid = match &self.jid {
            Some(jid) => jid,
            None => return,
        };

        let bare_jid = sender_jid.to_bare();

        // Get all connected resources for this user
        let resources = self.connection_registry.get_resources_for_user(&bare_jid);

        if resources.is_empty() {
            return;
        }

        debug!(
            resource_count = resources.len(),
            blocked_count = blocked_jids.len(),
            "Sending block push to connected resources"
        );

        for resource_jid in resources {
            let push = build_block_push(&resource_jid.clone().into(), blocked_jids);

            let stanza = Stanza::Iq(push);
            let result = self
                .connection_registry
                .send_to(&resource_jid, stanza)
                .await;

            match result {
                SendResult::Sent => {
                    debug!(to = %resource_jid, "Block push sent");
                }
                SendResult::NotConnected => {
                    debug!(to = %resource_jid, "Resource not connected for block push");
                }
                SendResult::ChannelFull | SendResult::ChannelClosed => {
                    warn!(to = %resource_jid, "Failed to send block push");
                }
            }
        }
    }

    /// Send unblock push notifications to all of the user's connected resources.
    ///
    /// Per XEP-0191, when a client unblocks a JID, the server MUST send
    /// a push notification to all of the user's connected resources.
    async fn send_unblock_push(&self, unblocked_jids: &[String]) {
        let sender_jid = match &self.jid {
            Some(jid) => jid,
            None => return,
        };

        let bare_jid = sender_jid.to_bare();

        // Get all connected resources for this user
        let resources = self.connection_registry.get_resources_for_user(&bare_jid);

        if resources.is_empty() {
            return;
        }

        debug!(
            resource_count = resources.len(),
            unblocked_count = unblocked_jids.len(),
            "Sending unblock push to connected resources"
        );

        for resource_jid in resources {
            let push = build_unblock_push(&resource_jid.clone().into(), unblocked_jids);

            let stanza = Stanza::Iq(push);
            let result = self
                .connection_registry
                .send_to(&resource_jid, stanza)
                .await;

            match result {
                SendResult::Sent => {
                    debug!(to = %resource_jid, "Unblock push sent");
                }
                SendResult::NotConnected => {
                    debug!(to = %resource_jid, "Resource not connected for unblock push");
                }
                SendResult::ChannelFull | SendResult::ChannelClosed => {
                    warn!(to = %resource_jid, "Failed to send unblock push");
                }
            }
        }
    }
}

fn isr_token_in_sasl_success_enabled() -> bool {
    let value = match std::env::var("WADDLE_XMPP_ISR_IN_SASL_SUCCESS") {
        Ok(value) => value,
        Err(_) => return true,
    };

    !matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
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

    /// Convert the stanza to a minidom Element.
    pub fn to_element(&self) -> minidom::Element {
        match self {
            Stanza::Message(m) => m.clone().into(),
            Stanza::Presence(p) => p.clone().into(),
            Stanza::Iq(i) => i.clone().into(),
        }
    }
}
