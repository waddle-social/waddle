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
pub mod connection;
pub mod mam;
pub mod metrics;
pub mod muc;
pub mod parser;
pub mod presence;
pub mod s2s;
pub mod server;
pub mod stream;

mod error;
mod types;

pub use error::{
    generate_iq_error, generate_stream_error, stream_errors, StanzaErrorCondition,
    StanzaErrorType, XmppError,
};
pub use parser::{ns, StreamHeader};
pub use server::{XmppServer, XmppServerConfig};
pub use types::*;

use std::sync::Arc;

/// Shared application state passed to the XMPP server.
///
/// This trait allows `waddle-server` to provide access to sessions,
/// permissions, and databases without circular dependencies.
pub trait AppState: Send + Sync + 'static {
    /// Validate an XMPP session token and return the associated session.
    fn validate_session(
        &self,
        jid: &jid::Jid,
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

    /// List all relations a subject has on an object.
    ///
    /// Used for deriving MUC affiliations from multiple permission relations.
    /// Returns a list of relation names (e.g., ["owner", "member"]).
    fn list_relations(
        &self,
        resource: &str,
        subject: &str,
    ) -> impl std::future::Future<Output = Result<Vec<String>, XmppError>> + Send;
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

/// Start the XMPP server with the given configuration and application state.
pub async fn start<S: AppState>(
    config: XmppServerConfig,
    app_state: Arc<S>,
) -> Result<XmppServer<S>, XmppError> {
    XmppServer::new(config, app_state).await
}
