//! Authentication Module
//!
//! This module implements authentication for Waddle, supporting both:
//! - ATProto OAuth authentication for Bluesky/ATProto users
//! - Native XMPP authentication via XEP-0077 In-Band Registration
//!
//! # ATProto OAuth Flow
//!
//! 1. Client provides a Bluesky handle (e.g., `user.bsky.social`)
//! 2. Server resolves the handle to a DID (Decentralized Identifier)
//! 3. Server fetches the DID document to discover the user's PDS (Personal Data Server)
//! 4. Server discovers the OAuth authorization server from the PDS
//! 5. Server initiates OAuth flow with PKCE
//! 6. User authenticates with their PDS
//! 7. Server exchanges authorization code for tokens
//! 8. Session is created and stored
//!
//! # Native XMPP Authentication
//!
//! Native users can register via XEP-0077 In-Band Registration and authenticate
//! using SCRAM-SHA-256. This provides a fallback for users without ATProto accounts.
//!
//! # Architecture
//!
//! - `did`: DID resolution (handle -> DID, DID document retrieval)
//! - `atproto`: OAuth flow implementation (authorization, token exchange)
//! - `session`: Session management and storage
//! - `jid`: DID to JID conversion for XMPP authentication
//! - `native`: Native user storage with Argon2id hashing for XEP-0077

pub mod atproto;
pub mod did;
pub mod dpop;
pub mod jid;
pub mod native;
pub mod session;

use thiserror::Error;

pub use atproto::AtprotoOAuth;
pub use jid::{did_to_jid, jid_to_did};
pub use native::{NativeUserStore, RegisterRequest};
pub use session::{Session, SessionManager};

/// Authentication-related errors
#[derive(Error, Debug)]
pub enum AuthError {
    #[error("Invalid handle format: {0}")]
    InvalidHandle(String),

    #[error("DID resolution failed: {0}")]
    DidResolutionFailed(String),

    #[error("DID document fetch failed: {0}")]
    DidDocumentFetchFailed(String),

    #[error("OAuth server discovery failed: {0}")]
    OAuthDiscoveryFailed(String),

    #[error("OAuth authorization failed: {0}")]
    OAuthAuthorizationFailed(String),

    #[error("Token exchange failed: {0}")]
    TokenExchangeFailed(String),

    #[error("Session not found: {0}")]
    SessionNotFound(String),

    #[error("Session expired")]
    SessionExpired,

    #[error("Invalid state parameter")]
    InvalidState,

    #[error("Database error: {0}")]
    DatabaseError(String),

    #[error("HTTP request failed: {0}")]
    HttpError(String),

    #[error("DNS resolution failed: {0}")]
    DnsError(String),

    #[error("Invalid DID: {0}")]
    InvalidDid(String),

    // Native user authentication errors (XEP-0077)
    #[error("User already exists: {0}")]
    UserAlreadyExists(String),

    #[error("User not found: {0}")]
    UserNotFound(String),

    #[error("Invalid username: {0}")]
    InvalidUsername(String),

    #[error("Invalid password: {0}")]
    InvalidPassword(String),

    #[error("Cryptographic error: {0}")]
    CryptoError(String),

    #[error("Registration disabled")]
    RegistrationDisabled,
}

impl From<reqwest::Error> for AuthError {
    fn from(err: reqwest::Error) -> Self {
        AuthError::HttpError(err.to_string())
    }
}
