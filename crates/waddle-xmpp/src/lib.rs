//! # waddle-xmpp
//!
//! Native XMPP server library for Waddle Social.
//!
//! This crate implements an XMPP server following RFC 6120/6121 and relevant XEPs,
//! designed to be embedded in `waddle-server` for unified deployment.
//!
//! ## Architecture
//!
//! - **Server**: TCP listener on port 5222 (C2S) and 5269 (S2S, future)
//! - **Connection Actors**: Each client connection managed by a Kameo actor
//! - **MUC Room Actors**: Multi-user chat rooms as separate actors
//! - **Stream Processing**: XML stream parsing via xmpp-parsers
//!
//! ## XEP Support
//!
//! MVP:
//! - RFC 6120/6121 (XMPP Core/IM)
//! - XEP-0030 (Service Discovery)
//! - XEP-0045 (Multi-User Chat)
//! - XEP-0198 (Stream Management)
//! - XEP-0280 (Message Carbons)
//! - XEP-0313 (Message Archive Management)

pub mod auth;
pub mod c2s;
pub mod carbons;
pub mod connection;
pub mod disco;
pub mod isr;
pub mod mam;
pub mod metrics;
pub mod muc;
pub mod parser;
pub mod presence;
pub mod pubsub;
pub mod registry;
pub mod roster;
pub mod routing;
pub mod s2s;
pub mod server;
pub mod stream;
pub mod stream_management;
pub mod xep;

mod error;
mod types;

pub use error::{
    generate_iq_error, generate_stream_error, stream_errors, StanzaErrorCondition, StanzaErrorType,
    XmppError,
};
pub use parser::{ns, StreamHeader};
pub use routing::{RouterConfig, RoutingDestination, RoutingResult, StanzaRouter};
pub use server::{XmppServer, XmppServerConfig};
pub use stream::{PreAuthResult, SaslAuthResult};
pub use types::*;
pub use xep::xep0077::{RegistrationError, RegistrationRequest};

use std::sync::Arc;

/// Shared application state passed to the XMPP server.
///
/// This trait allows `waddle-server` to provide access to sessions,
/// permissions, and databases without circular dependencies.
pub trait AppState: Send + Sync + 'static {
    /// Validate an XMPP session token with a JID and return the associated session.
    ///
    /// Used for PLAIN authentication where both JID and token are provided.
    fn validate_session(
        &self,
        jid: &jid::Jid,
        token: &str,
    ) -> impl std::future::Future<Output = Result<Session, XmppError>> + Send;

    /// Validate an XMPP session token without a JID and return the associated session.
    ///
    /// Used for OAUTHBEARER authentication where only the token is provided.
    /// The session lookup derives the JID from the token/session.
    fn validate_session_token(
        &self,
        token: &str,
    ) -> impl std::future::Future<Output = Result<Session, XmppError>> + Send;

    /// Check if a user has permission to perform an action.
    fn check_permission(
        &self,
        resource: &str,
        action: &str,
        subject: &str,
    ) -> impl std::future::Future<Output = Result<bool, XmppError>> + Send;

    /// Get the domain for this XMPP server.
    fn domain(&self) -> &str;

    /// Get the OAuth discovery URL for XMPP OAUTHBEARER (XEP-0493).
    ///
    /// This URL is sent to clients that request OAuth discovery.
    /// Should point to the RFC 8414 OAuth authorization server metadata endpoint.
    fn oauth_discovery_url(&self) -> String;

    /// List all relations a subject has on an object.
    ///
    /// Used for deriving MUC affiliations from multiple permission relations.
    /// Returns a list of relation names (e.g., ["owner", "member"]).
    fn list_relations(
        &self,
        resource: &str,
        subject: &str,
    ) -> impl std::future::Future<Output = Result<Vec<String>, XmppError>> + Send;

    /// List all subjects with a specific relation on an object.
    ///
    /// Used for MUC affiliation list queries (XEP-0045).
    /// Returns a list of (subject_id, relation) pairs.
    fn list_subjects(
        &self,
        resource: &str,
        relation: &str,
    ) -> impl std::future::Future<Output = Result<Vec<String>, XmppError>> + Send;

    /// Lookup SCRAM credentials for a native JID user.
    ///
    /// Used for SCRAM-SHA-256 authentication (RFC 5802/7677).
    /// Returns (StoredKey, ServerKey, salt_b64, iterations) if the user exists.
    ///
    /// For ATProto-only deployments, this can return None to indicate
    /// native JID authentication is not supported.
    fn lookup_scram_credentials(
        &self,
        username: &str,
    ) -> impl std::future::Future<Output = Result<Option<ScramCredentials>, XmppError>> + Send;

    /// Register a new native user via XEP-0077 In-Band Registration.
    ///
    /// Creates a new user account with the given credentials. The password
    /// will be securely hashed and SCRAM keys will be derived for authentication.
    ///
    /// # Arguments
    /// * `username` - The desired username (local part of JID)
    /// * `password` - The user's password (will be hashed)
    /// * `email` - Optional email address for account recovery
    ///
    /// # Returns
    /// * `Ok(())` on successful registration
    /// * `Err(XmppError)` if registration fails (user exists, invalid username, etc.)
    ///
    /// # Errors
    /// * Returns `XmppError::conflict` if username already exists
    /// * Returns `XmppError::not_acceptable` if username is invalid
    /// * Returns `XmppError::not_allowed` if registration is disabled
    fn register_native_user(
        &self,
        username: &str,
        password: &str,
        email: Option<&str>,
    ) -> impl std::future::Future<Output = Result<(), XmppError>> + Send;

    /// Check if a native user exists.
    ///
    /// Used by XEP-0077 registration to check for conflicts before creating users.
    fn native_user_exists(
        &self,
        username: &str,
    ) -> impl std::future::Future<Output = Result<bool, XmppError>> + Send;

    /// Get the vCard for a user (XEP-0054).
    ///
    /// Returns the stored vCard XML for the given bare JID, or None if no vCard exists.
    fn get_vcard(
        &self,
        jid: &jid::BareJid,
    ) -> impl std::future::Future<Output = Result<Option<String>, XmppError>> + Send;

    /// Store/update the vCard for a user (XEP-0054).
    ///
    /// Stores the vCard XML string for the given bare JID.
    fn set_vcard(
        &self,
        jid: &jid::BareJid,
        vcard_xml: &str,
    ) -> impl std::future::Future<Output = Result<(), XmppError>> + Send;

    /// Create an upload slot for XEP-0363 HTTP File Upload.
    ///
    /// Creates a new upload slot with the given parameters and returns the
    /// slot ID and URLs for uploading and retrieving the file.
    ///
    /// # Arguments
    /// * `requester_jid` - The JID of the user requesting the upload
    /// * `filename` - The original filename (will be sanitized)
    /// * `size` - The file size in bytes
    /// * `content_type` - Optional MIME content type
    ///
    /// # Returns
    /// * `Ok(UploadSlotInfo)` with PUT URL, GET URL, and optional headers
    /// * `Err(XmppError)` if slot creation fails
    fn create_upload_slot(
        &self,
        requester_jid: &jid::BareJid,
        filename: &str,
        size: u64,
        content_type: Option<&str>,
    ) -> impl std::future::Future<Output = Result<UploadSlotInfo, XmppError>> + Send;

    /// Get the maximum allowed file upload size in bytes.
    ///
    /// Returns the configured maximum file size for HTTP uploads.
    /// Default is 10 MB (10,485,760 bytes).
    fn max_upload_size(&self) -> u64 {
        10 * 1024 * 1024 // 10 MB default
    }

    /// Check if HTTP file upload is enabled.
    ///
    /// Returns true if the server supports XEP-0363 HTTP File Upload.
    fn upload_enabled(&self) -> bool {
        true // Enabled by default
    }

    // =========================================================================
    // RFC 6121 Roster Storage Methods
    // =========================================================================

    /// Get all roster items for a user.
    ///
    /// Returns all contacts in the user's roster.
    fn get_roster(
        &self,
        user_jid: &jid::BareJid,
    ) -> impl std::future::Future<Output = Result<Vec<roster::RosterItem>, XmppError>> + Send;

    /// Get a single roster item by JID.
    ///
    /// Returns the roster item if it exists, None otherwise.
    fn get_roster_item(
        &self,
        user_jid: &jid::BareJid,
        contact_jid: &jid::BareJid,
    ) -> impl std::future::Future<Output = Result<Option<roster::RosterItem>, XmppError>> + Send;

    /// Add or update a roster item.
    ///
    /// If the item already exists, it will be updated.
    /// If it doesn't exist, it will be created.
    fn set_roster_item(
        &self,
        user_jid: &jid::BareJid,
        item: &roster::RosterItem,
    ) -> impl std::future::Future<Output = Result<roster::RosterSetResult, XmppError>> + Send;

    /// Remove a roster item.
    ///
    /// Returns Ok(true) if the item was removed, Ok(false) if it didn't exist.
    fn remove_roster_item(
        &self,
        user_jid: &jid::BareJid,
        contact_jid: &jid::BareJid,
    ) -> impl std::future::Future<Output = Result<bool, XmppError>> + Send;

    /// Get the current roster version for a user.
    ///
    /// Roster versioning (XEP-0237) allows clients to efficiently sync
    /// their roster by only receiving changes since a known version.
    fn get_roster_version(
        &self,
        user_jid: &jid::BareJid,
    ) -> impl std::future::Future<Output = Result<Option<String>, XmppError>> + Send;

    /// Update the subscription state for a roster item.
    ///
    /// Creates the roster item if it doesn't exist.
    /// Returns the updated roster item.
    fn update_roster_subscription(
        &self,
        user_jid: &jid::BareJid,
        contact_jid: &jid::BareJid,
        subscription: roster::Subscription,
        ask: Option<roster::AskType>,
    ) -> impl std::future::Future<Output = Result<roster::RosterItem, XmppError>> + Send;

    /// Get all roster items where the user should send presence updates.
    ///
    /// Returns contacts with subscription=from or subscription=both.
    /// These are contacts who are subscribed to the user's presence.
    fn get_presence_subscribers(
        &self,
        user_jid: &jid::BareJid,
    ) -> impl std::future::Future<Output = Result<Vec<jid::BareJid>, XmppError>> + Send;

    /// Get all roster items where the user receives presence updates.
    ///
    /// Returns contacts with subscription=to or subscription=both.
    /// These are contacts whose presence the user is subscribed to.
    fn get_presence_subscriptions(
        &self,
        user_jid: &jid::BareJid,
    ) -> impl std::future::Future<Output = Result<Vec<jid::BareJid>, XmppError>> + Send;

    // =========================================================================
    // XEP-0191 Blocking Command Methods
    // =========================================================================

    /// Get all blocked JIDs for a user.
    ///
    /// Returns a list of bare JID strings that the user has blocked.
    fn get_blocklist(
        &self,
        user_jid: &jid::BareJid,
    ) -> impl std::future::Future<Output = Result<Vec<String>, XmppError>> + Send;

    /// Check if a JID is blocked by a user.
    ///
    /// Returns true if blocked_jid is on user_jid's blocklist.
    fn is_blocked(
        &self,
        user_jid: &jid::BareJid,
        blocked_jid: &jid::BareJid,
    ) -> impl std::future::Future<Output = Result<bool, XmppError>> + Send;

    /// Add JIDs to a user's blocklist.
    ///
    /// Returns the number of JIDs that were newly blocked (duplicates are ignored).
    fn add_blocks(
        &self,
        user_jid: &jid::BareJid,
        blocked_jids: &[String],
    ) -> impl std::future::Future<Output = Result<usize, XmppError>> + Send;

    /// Remove JIDs from a user's blocklist.
    ///
    /// Returns the number of JIDs that were removed.
    fn remove_blocks(
        &self,
        user_jid: &jid::BareJid,
        blocked_jids: &[String],
    ) -> impl std::future::Future<Output = Result<usize, XmppError>> + Send;

    /// Remove all JIDs from a user's blocklist.
    ///
    /// Returns the number of JIDs that were removed.
    fn remove_all_blocks(
        &self,
        user_jid: &jid::BareJid,
    ) -> impl std::future::Future<Output = Result<usize, XmppError>> + Send;

    // =========================================================================
    // XEP-0049 Private XML Storage Methods
    // =========================================================================

    /// Get private XML data for a user by namespace.
    ///
    /// Returns the stored XML string for the given bare JID and namespace,
    /// or None if no data exists for that namespace.
    fn get_private_xml(
        &self,
        jid: &jid::BareJid,
        namespace: &str,
    ) -> impl std::future::Future<Output = Result<Option<String>, XmppError>> + Send;

    /// Store/update private XML data for a user by namespace.
    ///
    /// Stores the XML string for the given bare JID and namespace.
    fn set_private_xml(
        &self,
        jid: &jid::BareJid,
        namespace: &str,
        xml_content: &str,
    ) -> impl std::future::Future<Output = Result<(), XmppError>> + Send;

    // =========================================================================
    // Auto-Join: Waddle & Channel Enumeration
    // =========================================================================

    /// List all waddles a user belongs to.
    ///
    /// Used for auto-joining all channels on login (Slack-like semantics).
    /// Returns a list of (waddle_id, waddle_name) pairs.
    fn list_user_waddles(
        &self,
        did: &str,
    ) -> impl std::future::Future<Output = Result<Vec<WaddleInfo>, XmppError>> + Send;

    /// List all channels in a waddle.
    ///
    /// Used for auto-joining all channels on login (Slack-like semantics).
    /// Returns a list of channel info for the given waddle.
    fn list_waddle_channels(
        &self,
        waddle_id: &str,
    ) -> impl std::future::Future<Output = Result<Vec<ChannelInfo>, XmppError>> + Send;
}

/// Basic waddle information for auto-join enumeration.
#[derive(Debug, Clone)]
pub struct WaddleInfo {
    /// Waddle ID
    pub id: String,
    /// Waddle name
    pub name: String,
}

/// Basic channel information for auto-join enumeration.
#[derive(Debug, Clone)]
pub struct ChannelInfo {
    /// Channel ID
    pub id: String,
    /// Channel name (used to derive MUC room JID local part)
    pub name: String,
    /// Channel type (e.g., "text", "voice")
    pub channel_type: String,
}

/// Information about a created upload slot (XEP-0363).
#[derive(Debug, Clone)]
pub struct UploadSlotInfo {
    /// URL for uploading the file (HTTP PUT).
    pub put_url: String,
    /// URL for retrieving the file (HTTP GET).
    pub get_url: String,
    /// Optional headers to include with the PUT request.
    pub put_headers: Vec<(String, String)>,
}

/// User session information.
#[derive(Debug, Clone)]
pub struct Session {
    /// ATProto DID
    pub did: String,
    /// XMPP JID (bare)
    pub jid: jid::BareJid,
    /// Session creation time
    pub created_at: chrono::DateTime<chrono::Utc>,
    /// Session expiration time
    pub expires_at: chrono::DateTime<chrono::Utc>,
}

/// SCRAM credentials for native JID authentication.
///
/// These are the stored values needed to verify SCRAM-SHA-256 authentication.
/// The password itself is never stored; only derived keys.
#[derive(Debug, Clone)]
pub struct ScramCredentials {
    /// StoredKey = H(ClientKey) where ClientKey = HMAC(SaltedPassword, "Client Key")
    pub stored_key: Vec<u8>,
    /// ServerKey = HMAC(SaltedPassword, "Server Key")
    pub server_key: Vec<u8>,
    /// Salt used for PBKDF2 (base64 encoded for storage)
    pub salt_b64: String,
    /// Number of PBKDF2 iterations used
    pub iterations: u32,
}

/// Start the XMPP server with the given configuration, state, listeners, and shutdown token.
pub async fn start<S: AppState>(
    config: XmppServerConfig,
    app_state: Arc<S>,
    c2s_listener: tokio::net::TcpListener,
    s2s_listener: Option<tokio::net::TcpListener>,
    shutdown_token: tokio_util::sync::CancellationToken,
) -> Result<XmppServer<S>, XmppError> {
    XmppServer::new(config, app_state, c2s_listener, s2s_listener, shutdown_token).await
}
