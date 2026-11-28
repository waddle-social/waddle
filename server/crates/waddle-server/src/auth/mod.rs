//! Authentication module.
//!
//! This module implements provider-based authentication for Waddle using:
//! - OIDC providers (discovery, ID token verification via JWKS)
//! - OAuth2 providers (token + userinfo)
//! - Local session management with UUID principals
//! - Optional native XMPP auth (XEP-0077/SCRAM)

pub mod directory;
pub mod identity;
pub mod identity_profile;
pub mod jid;
pub mod native;
pub mod oauth2;
pub mod oauth_grants;
pub mod oidc;
pub mod providers;
pub mod redirect;
pub mod retirement;
pub mod session;
pub mod session_endings;

use thiserror::Error;

pub use directory::{local_account_exists, local_account_jid_exists};
pub use identity::IdentityClaims;
pub use jid::{localpart_to_jid, username_to_localpart};
pub use native::{NativeUserStore, RegisterRequest};
pub use providers::AuthProviderTokenEndpointAuthMethod;
pub use providers::{
    AuthProviderConfig, AuthProviderKind, ProviderRegistry, TenantAuthProviderSpec,
};
pub use session::{Session, SessionManager};

/// Authentication-related errors.
#[derive(Error, Debug)]
pub enum AuthError {
    #[error("Invalid provider: {0}")]
    InvalidProvider(String),

    #[error("Invalid authentication request: {0}")]
    InvalidRequest(String),

    #[error("Invalid state parameter")]
    InvalidState,

    #[error("Invalid nonce")]
    InvalidNonce,

    #[error("Authorization failed: {0}")]
    AuthorizationFailed(String),

    #[error("Token exchange failed: {0}")]
    TokenExchangeFailed(String),

    #[error("User info fetch failed: {0}")]
    UserInfoFailed(String),

    #[error("JWT validation failed: {0}")]
    JwtError(String),

    /// Carries no session id: the id is the bearer credential, and this
    /// error is logged and returned to callers.
    #[error("Session not found")]
    SessionNotFound,

    #[error("Session expired")]
    SessionExpired,

    #[error("Database error: {0}")]
    DatabaseError(String),

    #[error("HTTP request failed: {0}")]
    HttpError(String),

    #[error("Identity provider unavailable (HTTP {0})")]
    ProviderUnreachable(reqwest::StatusCode),

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

    /// The JID belonged to an account that was retired. It is never issued
    /// to a new holder (ADR-0020 D8).
    #[error("The account JID is retired and is not issued again")]
    JidRetired,
}

impl From<reqwest::Error> for AuthError {
    fn from(err: reqwest::Error) -> Self {
        AuthError::HttpError(err.to_string())
    }
}

impl From<jsonwebtoken::errors::Error> for AuthError {
    fn from(err: jsonwebtoken::errors::Error) -> Self {
        AuthError::JwtError(err.to_string())
    }
}
