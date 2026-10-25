//! Provider-based authentication routes.
//!
//! API:
//! - GET /api/auth/providers
//! - GET /api/auth/start
//! - GET /api/auth/callback
//! - GET /api/auth/session
//! - POST /api/auth/logout

use crate::auth::identity::IdentityService;
use crate::auth::oauth2;
use crate::auth::oidc;
use crate::auth::{
    localpart_to_jid, AuthError, AuthProviderConfig, AuthProviderKind,
    AuthProviderTokenEndpointAuthMethod, ProviderRegistry, Session, SessionManager,
};
use crate::config::ServerConfig;
use crate::db::actor::DbActor;
use crate::db::actor::{DbExecute, DbQueryOne};
use crate::db::{row_value, ValueExt};
use crate::permissions::PermissionActor;
use crate::server::bootstrap_membership::{reconcile_user_membership, BootstrapMembershipConfig};
use crate::server::routes::auth_telemetry::{
    record_auth_failure, record_auth_success, AuthFailure,
};
use crate::server::AppState;
use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Json, Redirect, Response};
use axum::routing::{get, post};
use axum::Router;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};
use dashmap::DashMap;
use kameo::actor::ActorRef;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{instrument, warn};
use uuid::Uuid;
use waddle_xmpp::telemetry::attributes::AuthStage;

mod callback;
mod handshake;
mod state;

use callback::callback_handler;
pub use handshake::AuthHandshakeStore;
pub use state::{AuthState, ProfilePublishHook};

#[cfg(test)]
pub(crate) mod tests;

mod implementation;
pub(super) use implementation::auth_error_response;
pub(super) use implementation::auth_error_to_response;
pub use implementation::list_providers_handler;
pub use implementation::logout_handler;
pub use implementation::router;
pub use implementation::session_handler;
pub use implementation::start_handler;
pub use implementation::BrowserSessionTransport;
pub use implementation::CallbackQuery;
pub use implementation::DeviceAuthStatus;
pub use implementation::DeviceAuthorization;
pub use implementation::ErrorResponse;
pub use implementation::LogoutRequest;
pub use implementation::PendingAuthorization;
pub use implementation::PendingFlow;
pub use implementation::SessionQuery;
pub use implementation::SessionResponse;
pub use implementation::StartQuery;
pub use implementation::XmppAuthCode;
