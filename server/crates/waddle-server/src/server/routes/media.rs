//! Media session APIs backed by pluggable media backends.

use crate::auth::{AuthError, SessionManager};
use crate::media::{
    build_media_backend, MediaBackend, MediaBackendError, MediaConfig, MediaSessionRequest,
};
use crate::server::AppState;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{instrument, warn};

pub struct MediaState {
    pub session_manager: SessionManager,
    pub media_backend: Arc<dyn MediaBackend>,
}

impl MediaState {
    pub fn new(
        app_state: Arc<AppState>,
        encryption_key: Option<&[u8]>,
        media_config: &MediaConfig,
    ) -> Self {
        Self {
            session_manager: SessionManager::new(
                app_state.db_pool.global_actor().clone(),
                encryption_key,
            ),
            media_backend: build_media_backend(media_config),
        }
    }
}

pub fn router(state: Arc<MediaState>) -> Router {
    Router::new()
        .route("/v1/media/backend", get(media_backend_handler))
        .route("/v1/media/sessions", post(create_media_session_handler))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
pub struct SessionQuery {
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
pub struct CreateMediaSessionRequest {
    pub room_id: String,
    #[serde(default = "default_role")]
    pub role: String,
}

fn default_role() -> String {
    "publisher".to_string()
}

#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct MediaBackendResponse {
    pub backend: String,
}

fn media_error_to_response(err: MediaBackendError) -> (StatusCode, Json<ErrorResponse>) {
    match err {
        MediaBackendError::Disabled => (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(ErrorResponse {
                error: "media_disabled".to_string(),
                message: "media backend is disabled".to_string(),
            }),
        ),
        MediaBackendError::InvalidRequest(message) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse {
                error: "invalid_media_request".to_string(),
                message,
            }),
        ),
    }
}

fn auth_error_to_response(err: AuthError) -> (StatusCode, Json<ErrorResponse>) {
    match err {
        AuthError::SessionNotFound(_) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse {
                error: "session_not_found".to_string(),
                message: "session not found".to_string(),
            }),
        ),
        AuthError::SessionExpired => (
            StatusCode::UNAUTHORIZED,
            Json(ErrorResponse {
                error: "session_expired".to_string(),
                message: "session expired".to_string(),
            }),
        ),
        other => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "auth_error".to_string(),
                message: other.to_string(),
            }),
        ),
    }
}

#[instrument(skip(state))]
async fn media_backend_handler(State(state): State<Arc<MediaState>>) -> impl IntoResponse {
    (
        StatusCode::OK,
        Json(MediaBackendResponse {
            backend: state.media_backend.kind().to_string(),
        }),
    )
}

#[instrument(skip(state))]
async fn create_media_session_handler(
    State(state): State<Arc<MediaState>>,
    Query(params): Query<SessionQuery>,
    Json(request): Json<CreateMediaSessionRequest>,
) -> impl IntoResponse {
    let session = match state
        .session_manager
        .validate_session(&params.session_id)
        .await
    {
        Ok(session) => session,
        Err(err) => {
            warn!(error = %err, "media session auth failed");
            return auth_error_to_response(err).into_response();
        }
    };

    let media_request = MediaSessionRequest {
        room_id: request.room_id,
        participant_id: session.user_id,
        role: request.role,
    };

    match state.media_backend.create_session(media_request) {
        Ok(media_session) => (StatusCode::CREATED, Json(media_session)).into_response(),
        Err(err) => media_error_to_response(err).into_response(),
    }
}
