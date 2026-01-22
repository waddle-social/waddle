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
    generate_iq_error, generate_stream_error, stream_errors, StanzaErrorCondition,
    StanzaErrorType, XmppError,
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

/// Start the XMPP server with the given configuration and application state.
pub async fn start<S: AppState>(
    config: XmppServerConfig,
    app_state: Arc<S>,
) -> Result<XmppServer<S>, XmppError> {
    XmppServer::new(config, app_state).await
}
