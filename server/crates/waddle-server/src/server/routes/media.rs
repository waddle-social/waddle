//! Media session APIs backed by pluggable media backends.

mod call_registry;

use self::call_registry::{ActiveCall, ActiveCallRegistry, UpsertParticipant};
use crate::auth::{AuthError, Session, SessionManager};
use crate::media::{
    build_media_backend, MediaBackend, MediaBackendError, MediaConfig, MediaSession,
    MediaSessionRequest,
};
use crate::server::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
    Json, Router,
};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use tracing::{info, instrument, warn};
use uuid::Uuid;
use waddle_xmpp::prometheus;

pub struct MediaState {
    pub session_manager: SessionManager,
    pub media_backend: Arc<dyn MediaBackend>,
    pub call_registry: ActiveCallRegistry,
    abuse_protector: Arc<dyn MediaAbuseProtector>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum MediaMutationAction {
    CreateSession,
    JoinCall,
    LeaveCall,
}

#[derive(Debug, Clone)]
struct RateLimitRule {
    max_requests: u32,
    window: Duration,
}

#[derive(Debug, Clone)]
struct RateWindowCounter {
    window_started_at: DateTime<Utc>,
    count: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AbuseProtectionError {
    RateLimited,
}

trait MediaAbuseProtector: Send + Sync {
    fn check_mutation(
        &self,
        action: MediaMutationAction,
        session: &Session,
    ) -> Result<(), AbuseProtectionError>;
}

struct InMemoryMediaAbuseProtector {
    rules: HashMap<MediaMutationAction, RateLimitRule>,
    counters: Mutex<HashMap<(String, MediaMutationAction), RateWindowCounter>>,
}

impl Default for InMemoryMediaAbuseProtector {
    fn default() -> Self {
        Self {
            rules: HashMap::from([
                (
                    MediaMutationAction::CreateSession,
                    RateLimitRule {
                        max_requests: 30,
                        window: Duration::minutes(1),
                    },
                ),
                (
                    MediaMutationAction::JoinCall,
                    RateLimitRule {
                        max_requests: 60,
                        window: Duration::minutes(1),
                    },
                ),
                (
                    MediaMutationAction::LeaveCall,
                    RateLimitRule {
                        max_requests: 90,
                        window: Duration::minutes(1),
                    },
                ),
            ]),
            counters: Mutex::new(HashMap::new()),
        }
    }
}

impl MediaAbuseProtector for InMemoryMediaAbuseProtector {
    fn check_mutation(
        &self,
        action: MediaMutationAction,
        session: &Session,
    ) -> Result<(), AbuseProtectionError> {
        let Some(rule) = self.rules.get(&action) else {
            return Ok(());
        };

        let now = Utc::now();
        let mut counters = self
            .counters
            .lock()
            .expect("media abuse counters mutex should not be poisoned");
        let entry =
            counters
                .entry((session.user_id.clone(), action))
                .or_insert(RateWindowCounter {
                    window_started_at: now,
                    count: 0,
                });

        if now.signed_duration_since(entry.window_started_at) >= rule.window {
            entry.window_started_at = now;
            entry.count = 0;
        }

        if entry.count >= rule.max_requests {
            return Err(AbuseProtectionError::RateLimited);
        }

        entry.count += 1;
        Ok(())
    }
}

impl MediaState {
    pub fn new(
        app_state: Arc<AppState>,
        encryption_key: Option<&[u8]>,
        media_config: &MediaConfig,
    ) -> Self {
        Self::new_with_abuse_protector(
            app_state,
            encryption_key,
            media_config,
            Arc::new(InMemoryMediaAbuseProtector::default()),
        )
    }

    fn new_with_abuse_protector(
        app_state: Arc<AppState>,
        encryption_key: Option<&[u8]>,
        media_config: &MediaConfig,
        abuse_protector: Arc<dyn MediaAbuseProtector>,
    ) -> Self {
        Self {
            session_manager: SessionManager::new(
                app_state.db_pool.global_actor().clone(),
                encryption_key,
            ),
            media_backend: build_media_backend(media_config),
            call_registry: ActiveCallRegistry::new(),
            abuse_protector,
        }
    }
}

pub fn router(state: Arc<MediaState>) -> Router {
    Router::new()
        .route("/v1/media/backend", get(media_backend_handler))
        .route("/v1/media/sessions", post(create_media_session_handler))
        .route("/v1/media/calls", get(list_active_calls_handler))
        .route("/v1/media/calls/:call_id", get(get_call_details_handler))
        .route(
            "/v1/media/calls/:call_id/bootstrap",
            post(call_join_bootstrap_handler),
        )
        .route("/v1/media/calls/:call_id/leave", post(leave_call_handler))
        .with_state(state)
}

#[derive(Debug, Deserialize)]
pub struct SessionQuery {
    pub session_id: String,
}

#[derive(Debug, Deserialize)]
pub struct RoomCallsQuery {
    pub session_id: String,
    pub room_id: Option<String>,
    pub channel_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct CreateMediaSessionRequest {
    pub room_id: String,
    #[serde(default = "default_role")]
    pub role: String,
}

#[derive(Debug, Deserialize)]
pub struct JoinBootstrapRequest {
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

#[derive(Debug, Serialize)]
pub struct CreateMediaSessionResponse {
    pub call_id: String,
    #[serde(flatten)]
    pub media_session: MediaSession,
}

#[derive(Debug, Serialize)]
pub struct ActiveCallsResponse {
    pub room_id: String,
    pub calls: Vec<ActiveCall>,
}

#[derive(Debug, Serialize)]
pub struct CallBootstrapResponse {
    pub call: ActiveCall,
    pub media_session: MediaSession,
}

#[derive(Debug, Serialize)]
pub struct LeaveCallResponse {
    pub call_id: String,
    pub removed: bool,
    pub call: Option<ActiveCall>,
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

fn media_error_reason(err: &MediaBackendError) -> &'static str {
    match err {
        MediaBackendError::Disabled => "media_disabled",
        MediaBackendError::InvalidRequest(_) => "invalid_media_request",
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
        _other => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse {
                error: "auth_error".to_string(),
                message: "authentication failed".to_string(),
            }),
        ),
    }
}

fn abuse_error_to_response(err: AbuseProtectionError) -> (StatusCode, Json<ErrorResponse>) {
    match err {
        AbuseProtectionError::RateLimited => (
            StatusCode::TOO_MANY_REQUESTS,
            Json(ErrorResponse {
                error: "rate_limited".to_string(),
                message: "media mutation rate limit exceeded".to_string(),
            }),
        ),
    }
}

fn call_error_to_response(
    error: &str,
    message: impl Into<String>,
) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::NOT_FOUND,
        Json(ErrorResponse {
            error: error.to_string(),
            message: message.into(),
        }),
    )
}

fn internal_error_to_response(
    error: &str,
    message: impl Into<String>,
) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::INTERNAL_SERVER_ERROR,
        Json(ErrorResponse {
            error: error.to_string(),
            message: message.into(),
        }),
    )
}

fn bad_request_response(
    error: &str,
    message: impl Into<String>,
) -> (StatusCode, Json<ErrorResponse>) {
    (
        StatusCode::BAD_REQUEST,
        Json(ErrorResponse {
            error: error.to_string(),
            message: message.into(),
        }),
    )
}

fn observe_call_failure(
    operation: &str,
    reason: &str,
    call_id: Option<&str>,
    room_id: Option<&str>,
    participant_id: Option<&str>,
) {
    prometheus::record_call_failure(operation, reason);
    warn!(
        operation = operation,
        failure_reason = reason,
        call_id = call_id.unwrap_or(""),
        room_id = room_id.unwrap_or(""),
        participant_id = participant_id.unwrap_or(""),
        "call lifecycle operation failed"
    );
}

fn observe_call_duration(operation: &str, started_at: Instant) {
    prometheus::record_call_operation_duration(operation, started_at.elapsed().as_secs_f64());
}

fn normalize_non_empty_field(value: impl Into<String>, field_name: &str) -> Result<String, String> {
    let value = value.into();
    let trimmed = value.trim();
    if trimmed.is_empty() {
        return Err(format!("{field_name} cannot be empty"));
    }

    Ok(trimmed.to_string())
}

fn normalize_uuid_field(value: impl Into<String>, field_name: &str) -> Result<String, String> {
    let normalized = normalize_non_empty_field(value, field_name)?;
    Uuid::parse_str(&normalized).map_err(|_| format!("{field_name} must be a valid UUID"))?;
    Ok(normalized)
}

fn normalize_room_field(value: impl Into<String>, field_name: &str) -> Result<String, String> {
    let normalized = normalize_non_empty_field(value, field_name)?;
    if !normalized
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(format!(
            "{field_name} must only contain ASCII letters, numbers, '-' or '_'"
        ));
    }
    Ok(normalized)
}

fn normalize_role_field(value: impl Into<String>) -> Result<String, String> {
    let normalized = normalize_non_empty_field(value, "role")?.to_ascii_lowercase();
    match normalized.as_str() {
        "publisher" | "subscriber" => Ok(normalized),
        _ => Err("role must be one of: publisher, subscriber".to_string()),
    }
}

fn room_identifier(room_id: Option<String>, channel_id: Option<String>) -> Result<String, String> {
    let room = room_id
        .or(channel_id)
        .ok_or_else(|| "room_id or channel_id query parameter is required".to_string())?;

    normalize_room_field(room, "room_id")
}

async fn validate_session(
    session_manager: &SessionManager,
    session_id: &str,
) -> Result<Session, (StatusCode, Json<ErrorResponse>)> {
    let session_id = normalize_uuid_field(session_id, "session_id")
        .map_err(|message| bad_request_response("invalid_media_request", message))?;

    session_manager
        .validate_session(&session_id)
        .await
        .map_err(auth_error_to_response)
}

fn enforce_abuse_control(
    state: &MediaState,
    action: MediaMutationAction,
    session: &Session,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    state
        .abuse_protector
        .check_mutation(action, session)
        .map_err(abuse_error_to_response)
}

async fn register_call_participant(
    state: &MediaState,
    room_id: String,
    preferred_call_id: Option<String>,
    participant_id: String,
    media_session: &MediaSession,
    role: String,
) -> Result<ActiveCall, (StatusCode, Json<ErrorResponse>)> {
    state
        .call_registry
        .upsert_participant(UpsertParticipant {
            room_id,
            preferred_call_id,
            participant_id,
            backend_session_id: media_session.session_id.clone(),
            role,
            backend: media_session.backend.clone(),
            backend_room_id: media_session.room_id.clone(),
        })
        .await
        .ok_or_else(|| {
            internal_error_to_response(
                "call_registry_unavailable",
                "call registry is not available",
            )
        })
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

#[instrument(skip(state, params, request))]
async fn create_media_session_handler(
    State(state): State<Arc<MediaState>>,
    Query(params): Query<SessionQuery>,
    Json(request): Json<CreateMediaSessionRequest>,
) -> impl IntoResponse {
    let started_at = Instant::now();
    let session = match validate_session(&state.session_manager, &params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            observe_call_failure("create", "auth_failed", None, None, None);
            observe_call_duration("create", started_at);
            return err.into_response();
        }
    };

    if let Err(err) = enforce_abuse_control(&state, MediaMutationAction::CreateSession, &session) {
        observe_call_failure("create", "rate_limited", None, None, Some(&session.user_id));
        observe_call_duration("create", started_at);
        return err.into_response();
    }

    let room_id = match normalize_room_field(request.room_id, "room_id") {
        Ok(room_id) => room_id,
        Err(message) => {
            observe_call_failure(
                "create",
                "invalid_room_id",
                None,
                None,
                Some(&session.user_id),
            );
            observe_call_duration("create", started_at);
            return bad_request_response("invalid_media_request", message).into_response();
        }
    };

    let role = match normalize_role_field(request.role) {
        Ok(role) => role,
        Err(message) => {
            observe_call_failure(
                "create",
                "invalid_role",
                None,
                Some(&room_id),
                Some(&session.user_id),
            );
            observe_call_duration("create", started_at);
            return bad_request_response("invalid_media_request", message).into_response();
        }
    };

    let media_request = MediaSessionRequest {
        room_id: room_id.clone(),
        participant_id: session.user_id.clone(),
        role: role.clone(),
    };

    match state.media_backend.create_session(media_request) {
        Ok(media_session) => {
            let call = match register_call_participant(
                &state,
                room_id.clone(),
                None,
                session.user_id.clone(),
                &media_session,
                role.clone(),
            )
            .await
            {
                Ok(call) => call,
                Err(err) => {
                    observe_call_failure(
                        "create",
                        "call_registry_unavailable",
                        None,
                        Some(&room_id),
                        Some(&session.user_id),
                    );
                    observe_call_duration("create", started_at);
                    return err.into_response();
                }
            };

            observe_call_duration("create", started_at);
            info!(
                event = "call_create_succeeded",
                operation = "create",
                call_id = %call.call_id,
                room_id = %call.room_id,
                participant_id = %session.user_id,
                backend = %call.backend,
                role = %role,
                participant_count = call.participant_count,
                "call created"
            );

            (
                StatusCode::CREATED,
                Json(CreateMediaSessionResponse {
                    call_id: call.call_id,
                    media_session,
                }),
            )
                .into_response()
        }
        Err(err) => {
            observe_call_failure(
                "create",
                media_error_reason(&err),
                None,
                Some(&room_id),
                Some(&session.user_id),
            );
            observe_call_duration("create", started_at);
            media_error_to_response(err).into_response()
        }
    }
}

#[instrument(skip(state, params))]
async fn list_active_calls_handler(
    State(state): State<Arc<MediaState>>,
    Query(params): Query<RoomCallsQuery>,
) -> impl IntoResponse {
    if let Err(err) = validate_session(&state.session_manager, &params.session_id).await {
        warn!("active call list auth failed");
        return err.into_response();
    }

    let room_id = match room_identifier(params.room_id, params.channel_id) {
        Ok(room_id) => room_id,
        Err(message) => {
            return bad_request_response("invalid_media_request", message).into_response();
        }
    };

    let calls = match state.call_registry.list_calls_by_room(&room_id).await {
        Some(calls) => calls,
        None => {
            return internal_error_to_response(
                "call_registry_unavailable",
                "call registry is not available",
            )
            .into_response();
        }
    };

    (StatusCode::OK, Json(ActiveCallsResponse { room_id, calls })).into_response()
}

#[instrument(skip(state, params, call_id))]
async fn get_call_details_handler(
    State(state): State<Arc<MediaState>>,
    Path(call_id): Path<String>,
    Query(params): Query<SessionQuery>,
) -> impl IntoResponse {
    if let Err(err) = validate_session(&state.session_manager, &params.session_id).await {
        warn!("active call details auth failed");
        return err.into_response();
    }

    let call_id = match normalize_uuid_field(call_id, "call_id") {
        Ok(call_id) => call_id,
        Err(message) => {
            return bad_request_response("invalid_media_request", message).into_response();
        }
    };

    match state.call_registry.get_call(&call_id).await {
        Some(Some(call)) => (StatusCode::OK, Json(call)).into_response(),
        Some(None) => call_error_to_response("call_not_found", "call not found").into_response(),
        None => internal_error_to_response(
            "call_registry_unavailable",
            "call registry is not available",
        )
        .into_response(),
    }
}

#[instrument(skip(state, params, call_id, request))]
async fn call_join_bootstrap_handler(
    State(state): State<Arc<MediaState>>,
    Path(call_id): Path<String>,
    Query(params): Query<SessionQuery>,
    Json(request): Json<JoinBootstrapRequest>,
) -> impl IntoResponse {
    let started_at = Instant::now();
    let session = match validate_session(&state.session_manager, &params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            observe_call_failure("bootstrap", "auth_failed", Some(&call_id), None, None);
            observe_call_duration("bootstrap", started_at);
            return err.into_response();
        }
    };

    if let Err(err) = enforce_abuse_control(&state, MediaMutationAction::JoinCall, &session) {
        observe_call_failure(
            "bootstrap",
            "rate_limited",
            Some(&call_id),
            None,
            Some(&session.user_id),
        );
        observe_call_duration("bootstrap", started_at);
        return err.into_response();
    }

    let call_id = match normalize_uuid_field(call_id, "call_id") {
        Ok(call_id) => call_id,
        Err(message) => {
            observe_call_failure(
                "bootstrap",
                "invalid_call_id",
                None,
                None,
                Some(&session.user_id),
            );
            observe_call_duration("bootstrap", started_at);
            return bad_request_response("invalid_media_request", message).into_response();
        }
    };

    let call = match state.call_registry.get_call(&call_id).await {
        Some(Some(call)) => call,
        Some(None) => {
            observe_call_failure(
                "bootstrap",
                "call_not_found",
                Some(&call_id),
                None,
                Some(&session.user_id),
            );
            observe_call_duration("bootstrap", started_at);
            return call_error_to_response("call_not_found", "call not found").into_response();
        }
        None => {
            observe_call_failure(
                "bootstrap",
                "call_registry_unavailable",
                Some(&call_id),
                None,
                Some(&session.user_id),
            );
            observe_call_duration("bootstrap", started_at);
            return internal_error_to_response(
                "call_registry_unavailable",
                "call registry is not available",
            )
            .into_response();
        }
    };

    let role = match normalize_role_field(request.role) {
        Ok(role) => role,
        Err(message) => {
            observe_call_failure(
                "bootstrap",
                "invalid_role",
                Some(&call_id),
                Some(&call.room_id),
                Some(&session.user_id),
            );
            observe_call_duration("bootstrap", started_at);
            return bad_request_response("invalid_media_request", message).into_response();
        }
    };

    let media_request = MediaSessionRequest {
        room_id: call.room_id.clone(),
        participant_id: session.user_id.clone(),
        role: role.clone(),
    };

    let media_session = match state.media_backend.create_session(media_request) {
        Ok(session) => session,
        Err(err) => {
            observe_call_failure(
                "bootstrap",
                media_error_reason(&err),
                Some(&call_id),
                Some(&call.room_id),
                Some(&session.user_id),
            );
            observe_call_duration("bootstrap", started_at);
            return media_error_to_response(err).into_response();
        }
    };

    let call_room_id = call.room_id.clone();
    let preferred_call_id = call.call_id.clone();
    let participant_id = session.user_id.clone();
    let updated_call = match register_call_participant(
        &state,
        call_room_id.clone(),
        Some(preferred_call_id),
        participant_id.clone(),
        &media_session,
        role.clone(),
    )
    .await
    {
        Ok(call) => call,
        Err(err) => {
            observe_call_failure(
                "bootstrap",
                "call_registry_unavailable",
                Some(&call_id),
                Some(&call_room_id),
                Some(&participant_id),
            );
            observe_call_duration("bootstrap", started_at);
            return err.into_response();
        }
    };

    observe_call_duration("bootstrap", started_at);
    info!(
        event = "call_bootstrap_succeeded",
        operation = "bootstrap",
        call_id = %updated_call.call_id,
        room_id = %updated_call.room_id,
        participant_id = %session.user_id,
        backend = %updated_call.backend,
        role = %role,
        participant_count = updated_call.participant_count,
        "call bootstrap joined existing call"
    );

    (
        StatusCode::OK,
        Json(CallBootstrapResponse {
            call: updated_call,
            media_session,
        }),
    )
        .into_response()
}

#[instrument(skip(state, params, call_id))]
async fn leave_call_handler(
    State(state): State<Arc<MediaState>>,
    Path(call_id): Path<String>,
    Query(params): Query<SessionQuery>,
) -> impl IntoResponse {
    let started_at = Instant::now();
    let session = match validate_session(&state.session_manager, &params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            observe_call_failure("leave", "auth_failed", Some(&call_id), None, None);
            observe_call_duration("leave", started_at);
            return err.into_response();
        }
    };

    if let Err(err) = enforce_abuse_control(&state, MediaMutationAction::LeaveCall, &session) {
        observe_call_failure(
            "leave",
            "rate_limited",
            Some(&call_id),
            None,
            Some(&session.user_id),
        );
        observe_call_duration("leave", started_at);
        return err.into_response();
    }

    let call_id = match normalize_uuid_field(call_id, "call_id") {
        Ok(call_id) => call_id,
        Err(message) => {
            observe_call_failure(
                "leave",
                "invalid_call_id",
                None,
                None,
                Some(&session.user_id),
            );
            observe_call_duration("leave", started_at);
            return bad_request_response("invalid_media_request", message).into_response();
        }
    };

    let result = match state
        .call_registry
        .remove_participant(&call_id, &session.user_id)
        .await
    {
        Some(result) => result,
        None => {
            observe_call_failure(
                "leave",
                "call_registry_unavailable",
                Some(&call_id),
                None,
                Some(&session.user_id),
            );
            observe_call_duration("leave", started_at);
            return internal_error_to_response(
                "call_registry_unavailable",
                "call registry is not available",
            )
            .into_response();
        }
    };

    observe_call_duration("leave", started_at);
    info!(
        event = "call_leave_processed",
        operation = "leave",
        call_id = %call_id,
        participant_id = %session.user_id,
        removed = result.removed,
        active_participants = result
            .call
            .as_ref()
            .map(|call| call.participant_count as i64)
            .unwrap_or(0),
        "call leave processed"
    );

    (
        StatusCode::OK,
        Json(LeaveCallResponse {
            call_id,
            removed: result.removed,
            call: result.call,
        }),
    )
        .into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;
    use uuid::Uuid;

    async fn create_test_media_state() -> Arc<MediaState> {
        create_test_media_state_with_abuse_protector(Arc::new(
            InMemoryMediaAbuseProtector::default(),
        ))
        .await
    }

    async fn create_test_media_state_with_abuse_protector(
        abuse_protector: Arc<dyn MediaAbuseProtector>,
    ) -> Arc<MediaState> {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let db_pool = DatabasePool::new(config, pool_config).await.unwrap();

        let runner = MigrationRunner::global();
        runner.run(db_pool.global()).await.unwrap();

        let app_state = Arc::new(AppState::new(Arc::new(db_pool)));

        let mut media_config = MediaConfig::default();
        media_config.backend = crate::media::MediaBackendKind::WebrtcRsSfu;
        media_config.public_base_url = "https://calls.waddle.test".to_string();

        Arc::new(MediaState::new_with_abuse_protector(
            app_state,
            Some(b"test-encryption-key-32-bytes!!!"),
            &media_config,
            abuse_protector,
        ))
    }

    struct DenyAllAbuseProtector;

    impl MediaAbuseProtector for DenyAllAbuseProtector {
        fn check_mutation(
            &self,
            _action: MediaMutationAction,
            _session: &Session,
        ) -> Result<(), AbuseProtectionError> {
            Err(AbuseProtectionError::RateLimited)
        }
    }

    async fn create_test_session(state: &MediaState) -> Session {
        let user_id = Uuid::new_v4().to_string();
        let username = format!("test{}", &user_id[..8]);
        let session = Session::new(&user_id, &username, &username);

        state
            .session_manager
            .create_session(&session)
            .await
            .unwrap();

        session
    }

    #[tokio::test]
    async fn list_and_detail_active_calls_for_room() {
        let media_state = create_test_media_state().await;
        let session_1 = create_test_session(&media_state).await;
        let session_2 = create_test_session(&media_state).await;
        let app = router(media_state);

        let create_1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/media/sessions?session_id={}", session_1.id))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"room_id":"team-sync","role":"publisher"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_1.status(), StatusCode::CREATED);

        let create_1_body = create_1.into_body().collect().await.unwrap().to_bytes();
        let create_1_json: serde_json::Value = serde_json::from_slice(&create_1_body).unwrap();
        let call_id = create_1_json["call_id"].as_str().unwrap().to_string();

        let create_2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/media/sessions?session_id={}", session_2.id))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"room_id":"team-sync","role":"subscriber"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_2.status(), StatusCode::CREATED);

        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/media/calls?session_id={}&room_id=team-sync",
                        session_1.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body = list_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let list_json: serde_json::Value = serde_json::from_slice(&list_body).unwrap();
        assert_eq!(list_json["calls"].as_array().unwrap().len(), 1);
        assert_eq!(list_json["calls"][0]["participant_count"], 2);

        let detail_response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/media/calls/{}?session_id={}",
                        call_id, session_1.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(detail_response.status(), StatusCode::OK);
        let detail_body = detail_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let detail_json: serde_json::Value = serde_json::from_slice(&detail_body).unwrap();
        assert_eq!(detail_json["call_id"], call_id);
        assert_eq!(detail_json["participant_count"], 2);
    }

    #[tokio::test]
    async fn call_routes_require_valid_session() {
        let media_state = create_test_media_state().await;
        let app = router(media_state);
        let missing_session_id = Uuid::new_v4().to_string();
        let call_id = Uuid::new_v4().to_string();

        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/media/calls?session_id={}&room_id=team-sync",
                        missing_session_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(list_response.status(), StatusCode::NOT_FOUND);
        let list_body = list_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let list_json: serde_json::Value = serde_json::from_slice(&list_body).unwrap();
        assert_eq!(list_json["error"], "session_not_found");

        let detail_response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/media/calls/{}?session_id={}",
                        call_id, missing_session_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(detail_response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn call_details_return_not_found_for_unknown_call() {
        let media_state = create_test_media_state().await;
        let session = create_test_session(&media_state).await;
        let app = router(media_state);
        let unknown_call_id = Uuid::new_v4().to_string();

        let response = app
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/media/calls/{}?session_id={}",
                        unknown_call_id, session.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "call_not_found");
    }

    #[tokio::test]
    async fn call_bootstrap_joins_existing_call() {
        let media_state = create_test_media_state().await;
        let session_1 = create_test_session(&media_state).await;
        let session_2 = create_test_session(&media_state).await;
        let app = router(media_state);

        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/media/sessions?session_id={}", session_1.id))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"room_id":"daily","role":"publisher"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        let create_body = create_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let create_json: serde_json::Value = serde_json::from_slice(&create_body).unwrap();
        let created_call_id = create_json["call_id"].as_str().unwrap();

        let list_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/media/calls?session_id={}&channel_id=daily",
                        session_2.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(list_response.status(), StatusCode::OK);
        let list_body = list_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let list_json: serde_json::Value = serde_json::from_slice(&list_body).unwrap();
        let listed_call_id = list_json["calls"][0]["call_id"].as_str().unwrap();
        assert_eq!(listed_call_id, created_call_id);

        let bootstrap_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/media/calls/{}/bootstrap?session_id={}",
                        listed_call_id, session_2.id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"role":"subscriber"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(bootstrap_response.status(), StatusCode::OK);
        let bootstrap_body = bootstrap_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let bootstrap_json: serde_json::Value = serde_json::from_slice(&bootstrap_body).unwrap();
        assert_eq!(bootstrap_json["call"]["call_id"], listed_call_id);
        assert_eq!(bootstrap_json["call"]["participant_count"], 2);
        assert_eq!(bootstrap_json["media_session"]["role"], "subscriber");
    }

    #[tokio::test]
    async fn create_media_session_rejects_empty_room_or_role() {
        let media_state = create_test_media_state().await;
        let session = create_test_session(&media_state).await;
        let app = router(media_state);

        let empty_room = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/media/sessions?session_id={}", session.id))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"room_id":"   ","role":"publisher"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(empty_room.status(), StatusCode::BAD_REQUEST);

        let empty_role = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/media/sessions?session_id={}", session.id))
                    .header("content-type", "application/json")
                    .body(Body::from("{\"room_id\":\"team-sync\",\"role\":\"   \"}"))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(empty_role.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn create_media_session_rejects_invalid_identifiers() {
        let media_state = create_test_media_state().await;
        let session = create_test_session(&media_state).await;
        let app = router(media_state);

        let invalid_session = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/media/sessions?session_id=not-a-uuid")
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"room_id":"team-sync","role":"publisher"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid_session.status(), StatusCode::BAD_REQUEST);

        let invalid_room = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/media/sessions?session_id={}", session.id))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"room_id":"team/sync","role":"publisher"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(invalid_room.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn call_routes_reject_invalid_call_identifier() {
        let media_state = create_test_media_state().await;
        let session = create_test_session(&media_state).await;
        let app = router(media_state);

        let detail = app
            .clone()
            .oneshot(
                Request::builder()
                    .uri(format!(
                        "/v1/media/calls/not-a-uuid?session_id={}",
                        session.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(detail.status(), StatusCode::BAD_REQUEST);

        let bootstrap = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/media/calls/not-a-uuid/bootstrap?session_id={}",
                        session.id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"role":"subscriber"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bootstrap.status(), StatusCode::BAD_REQUEST);

        let leave = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/media/calls/not-a-uuid/leave?session_id={}",
                        session.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(leave.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn media_mutations_apply_rate_limit_hook() {
        let media_state =
            create_test_media_state_with_abuse_protector(Arc::new(DenyAllAbuseProtector)).await;
        let session = create_test_session(&media_state).await;
        let app = router(media_state);

        let create = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/media/sessions?session_id={}", session.id))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"room_id":"daily","role":"publisher"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create.status(), StatusCode::TOO_MANY_REQUESTS);

        let blocked_call_id = Uuid::new_v4().to_string();
        let bootstrap = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/media/calls/{}/bootstrap?session_id={}",
                        blocked_call_id, session.id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"role":"subscriber"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(bootstrap.status(), StatusCode::TOO_MANY_REQUESTS);

        let leave = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/media/calls/{}/leave?session_id={}",
                        blocked_call_id, session.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(leave.status(), StatusCode::TOO_MANY_REQUESTS);
    }

    #[tokio::test]
    async fn call_bootstrap_rejects_empty_role() {
        let media_state = create_test_media_state().await;
        let session_1 = create_test_session(&media_state).await;
        let session_2 = create_test_session(&media_state).await;
        let app = router(media_state);

        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/media/sessions?session_id={}", session_1.id))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"room_id":"daily","role":"publisher"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let create_body = create_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let create_json: serde_json::Value = serde_json::from_slice(&create_body).unwrap();
        let call_id = create_json["call_id"].as_str().unwrap();

        let bootstrap_response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/media/calls/{}/bootstrap?session_id={}",
                        call_id, session_2.id
                    ))
                    .header("content-type", "application/json")
                    .body(Body::from("{\"role\":\"  \"}"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(bootstrap_response.status(), StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn leave_call_removes_participant_and_is_idempotent() {
        let media_state = create_test_media_state().await;
        let session_1 = create_test_session(&media_state).await;
        let session_2 = create_test_session(&media_state).await;
        let app = router(media_state);

        let create_1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/media/sessions?session_id={}", session_1.id))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"room_id":"daily","role":"publisher"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        let create_1_body = create_1.into_body().collect().await.unwrap().to_bytes();
        let create_1_json: serde_json::Value = serde_json::from_slice(&create_1_body).unwrap();
        let call_id = create_1_json["call_id"].as_str().unwrap().to_string();

        let create_2 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/media/sessions?session_id={}", session_2.id))
                    .header("content-type", "application/json")
                    .body(Body::from(r#"{"room_id":"daily","role":"subscriber"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_2.status(), StatusCode::CREATED);

        let leave_once = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/media/calls/{}/leave?session_id={}",
                        call_id, session_2.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(leave_once.status(), StatusCode::OK);
        let leave_once_body = leave_once.into_body().collect().await.unwrap().to_bytes();
        let leave_once_json: serde_json::Value = serde_json::from_slice(&leave_once_body).unwrap();
        assert_eq!(leave_once_json["removed"], true);
        assert_eq!(leave_once_json["call"]["participant_count"], 1);

        let leave_again = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/media/calls/{}/leave?session_id={}",
                        call_id, session_2.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(leave_again.status(), StatusCode::OK);
        let leave_again_body = leave_again.into_body().collect().await.unwrap().to_bytes();
        let leave_again_json: serde_json::Value =
            serde_json::from_slice(&leave_again_body).unwrap();
        assert_eq!(leave_again_json["removed"], false);
        assert_eq!(leave_again_json["call"]["participant_count"], 1);

        let leave_last = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/media/calls/{}/leave?session_id={}",
                        call_id, session_1.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(leave_last.status(), StatusCode::OK);
        let leave_last_body = leave_last.into_body().collect().await.unwrap().to_bytes();
        let leave_last_json: serde_json::Value = serde_json::from_slice(&leave_last_body).unwrap();
        assert_eq!(leave_last_json["removed"], true);
        assert!(leave_last_json["call"].is_null());
    }
}
