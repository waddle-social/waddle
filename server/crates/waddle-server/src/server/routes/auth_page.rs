//! Deprecated web auth page routes.
//!
//! This release intentionally disables the legacy auth page surface.

use super::auth::AuthState;
use axum::{
    extract::State,
    http::StatusCode,
    response::{Html, IntoResponse},
    routing::get,
    Router,
};
use std::sync::Arc;

pub fn router(auth_state: Arc<AuthState>) -> Router {
    Router::new()
        .route("/auth", get(disabled_handler))
        .route("/auth/start", get(disabled_handler))
        .route("/auth/callback", get(disabled_handler))
        .with_state(auth_state)
}

async fn disabled_handler(State(_state): State<Arc<AuthState>>) -> impl IntoResponse {
    (
        StatusCode::GONE,
        Html("<html><body><h1>410 Gone</h1><p>Legacy auth pages are disabled. Use /v2/auth/providers and /v2/auth/start.</p></body></html>".to_string()),
    )
}
