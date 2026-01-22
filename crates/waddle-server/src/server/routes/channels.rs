//! Channel CRUD API Routes
//!
//! Provides HTTP endpoints for managing Channels within Waddles:
//! - POST /v1/waddles/:wid/channels - Create a new channel in a waddle
//! - GET /v1/waddles/:wid/channels - List channels in a waddle
//! - GET /v1/channels/:id - Get channel details (requires waddle_id query param)
//! - PATCH /v1/channels/:id - Update channel metadata
//! - DELETE /v1/channels/:id - Delete a channel

use crate::auth::{AuthError, SessionManager};
use crate::db::Database;
use crate::permissions::{
    Object, ObjectType, PermissionError, PermissionService, Relation, Subject, SubjectType, Tuple,
};
use crate::server::AppState;
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, patch, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};
use uuid::Uuid;

/// Extended application state for channel routes
pub struct ChannelState {
    /// Core app state
    pub app_state: Arc<AppState>,
    /// Permission service (uses global DB)
    pub permission_service: PermissionService,
    /// Session manager
    pub session_manager: SessionManager,
}

impl ChannelState {
    /// Create new channel state
    pub fn new(app_state: Arc<AppState>, encryption_key: Option<&[u8]>) -> Self {
        let db = Arc::new(app_state.db_pool.global().clone());
        let permission_service = PermissionService::new(Arc::clone(&db));
        let session_manager = SessionManager::new(Arc::clone(&db), encryption_key);
        Self {
            app_state,
            permission_service,
            session_manager,
        }
    }
}

/// Create the channels router
pub fn router(channel_state: Arc<ChannelState>) -> Router {
    Router::new()
        // Waddle-scoped routes
        .route(
            "/v1/waddles/:waddle_id/channels",
            post(create_channel_handler),
        )
        .route("/v1/waddles/:waddle_id/channels", get(list_channels_handler))
        // Channel-scoped routes (require waddle_id in query params)
        .route("/v1/channels/:id", get(get_channel_handler))
        .route("/v1/channels/:id", patch(update_channel_handler))
        .route("/v1/channels/:id", delete(delete_channel_handler))
        .with_state(channel_state)
}

// === Request/Response Types ===

/// Request body for creating a new channel
#[derive(Debug, Deserialize)]
pub struct CreateChannelRequest {
    /// Channel name (required)
    pub name: String,
    /// Channel description (optional)
    pub description: Option<String>,
    /// Channel type (default: "text")
    #[serde(default = "default_channel_type")]
    pub channel_type: String,
    /// Position in channel list (default: 0)
    #[serde(default)]
    pub position: i32,
}

fn default_channel_type() -> String {
    "text".to_string()
}

/// Request body for updating a channel
#[derive(Debug, Deserialize)]
pub struct UpdateChannelRequest {
    /// New channel name (optional)
    pub name: Option<String>,
    /// New description (optional)
    pub description: Option<String>,
    /// New position (optional)
    pub position: Option<i32>,
}

/// Response for a single channel
#[derive(Debug, Serialize)]
pub struct ChannelResponse {
    /// Channel ID
    pub id: String,
    /// Waddle ID this channel belongs to
    pub waddle_id: String,
    /// Channel name
    pub name: String,
    /// Channel description
    pub description: Option<String>,
    /// Channel type (text, voice, etc.)
    pub channel_type: String,
    /// Position in channel list
    pub position: i32,
    /// Whether this is the default channel
    pub is_default: bool,
    /// When the channel was created
    pub created_at: String,
    /// When the channel was last updated
    pub updated_at: Option<String>,
}

/// Response for list of channels
#[derive(Debug, Serialize)]
pub struct ListChannelsResponse {
    /// List of channels
    pub channels: Vec<ChannelResponse>,
    /// Total count
    pub total: usize,
}

/// Query parameters for session authentication
#[derive(Debug, Deserialize)]
pub struct SessionQuery {
    /// Session ID for authentication
    pub session_id: String,
}

/// Query parameters for getting/updating/deleting a channel
#[derive(Debug, Deserialize)]
pub struct ChannelQuery {
    /// Session ID for authentication
    pub session_id: String,
    /// Waddle ID the channel belongs to
    pub waddle_id: String,
}

/// Query parameters for listing channels
#[derive(Debug, Deserialize)]
pub struct ListChannelsQuery {
    /// Session ID for authentication
    pub session_id: String,
    /// Maximum number of results (default: 100)
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Offset for pagination (default: 0)
    #[serde(default)]
    pub offset: usize,
}

fn default_limit() -> usize {
    100
}

/// Error response
#[derive(Debug, Serialize)]
pub struct ErrorResponse {
    pub error: String,
    pub message: String,
}

impl ErrorResponse {
    fn new(error: &str, message: &str) -> Self {
        Self {
            error: error.to_string(),
            message: message.to_string(),
        }
    }
}

/// Channel-specific error type
#[derive(Debug)]
pub enum ChannelError {
    Auth(AuthError),
    Permission(PermissionError),
    NotFound(String),
    Database(String),
    InvalidInput(String),
}

impl From<AuthError> for ChannelError {
    fn from(err: AuthError) -> Self {
        ChannelError::Auth(err)
    }
}

impl From<PermissionError> for ChannelError {
    fn from(err: PermissionError) -> Self {
        ChannelError::Permission(err)
    }
}

/// Convert ChannelError to HTTP response
fn channel_error_to_response(err: ChannelError) -> (StatusCode, Json<ErrorResponse>) {
    match err {
        ChannelError::Auth(auth_err) => {
            let (status, error_code) = match &auth_err {
                AuthError::SessionNotFound(_) => (StatusCode::NOT_FOUND, "session_not_found"),
                AuthError::SessionExpired => (StatusCode::UNAUTHORIZED, "session_expired"),
                _ => (StatusCode::INTERNAL_SERVER_ERROR, "auth_error"),
            };
            (
                status,
                Json(ErrorResponse::new(error_code, &auth_err.to_string())),
            )
        }
        ChannelError::Permission(perm_err) => {
            let (status, error_code) = match &perm_err {
                PermissionError::Denied(_) => (StatusCode::FORBIDDEN, "permission_denied"),
                _ => (StatusCode::BAD_REQUEST, "permission_error"),
            };
            (
                status,
                Json(ErrorResponse::new(error_code, &perm_err.to_string())),
            )
        }
        ChannelError::NotFound(msg) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::new("not_found", &msg)),
        ),
        ChannelError::Database(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new("database_error", &msg)),
        ),
        ChannelError::InvalidInput(msg) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new("invalid_input", &msg)),
        ),
    }
}

// === Handlers ===

/// POST /v1/waddles/:waddle_id/channels
///
/// Create a new channel in a waddle. Requires create_channel permission on the waddle.
#[instrument(skip(state))]
pub async fn create_channel_handler(
    State(state): State<Arc<ChannelState>>,
    Path(waddle_id): Path<String>,
    Query(params): Query<SessionQuery>,
    Json(request): Json<CreateChannelRequest>,
) -> impl IntoResponse {
    info!("Creating channel '{}' in waddle {}", request.name, waddle_id);

    // Validate session
    let session = match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return channel_error_to_response(ChannelError::Auth(err)).into_response();
        }
    };

    // Validate input
    if request.name.trim().is_empty() {
        return channel_error_to_response(ChannelError::InvalidInput(
            "Channel name cannot be empty".to_string(),
        ))
        .into_response();
    }

    // Check if user has permission to create channels in this waddle
    let subject = Subject::user(&session.did);
    let waddle_object = Object::new(ObjectType::Waddle, &waddle_id);

    let can_create = state
        .permission_service
        .check(&subject, "create_channel", &waddle_object)
        .await
        .map(|r| r.allowed)
        .unwrap_or(false);

    if !can_create {
        return channel_error_to_response(ChannelError::Permission(PermissionError::Denied(
            "You do not have permission to create channels in this waddle".to_string(),
        )))
        .into_response();
    }

    // Get or create the waddle database
    let waddle_db = match state.app_state.db_pool.get_waddle_db(&waddle_id).await {
        Ok(db) => db,
        Err(err) => {
            error!("Failed to get waddle database: {}", err);
            return channel_error_to_response(ChannelError::Database(format!(
                "Failed to access waddle database: {}",
                err
            )))
            .into_response();
        }
    };

    // Generate channel ID
    let channel_id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    // Insert channel into database
    if let Err(err) = insert_channel(
        &waddle_db,
        &channel_id,
        &request.name,
        request.description.as_deref(),
        &request.channel_type,
        request.position,
        false, // not default
        &now,
    )
    .await
    {
        error!("Failed to insert channel: {}", err);
        return channel_error_to_response(ChannelError::Database(err)).into_response();
    }

    // Create permission tuple: channel#parent@waddle
    // This establishes the parent relationship so channel permissions can inherit from waddle
    let parent_tuple = Tuple::new(
        Object::new(ObjectType::Channel, &channel_id),
        Relation::new("parent"),
        Subject {
            subject_type: SubjectType::Waddle,
            id: waddle_id.clone(),
            relation: None,
        },
    );

    if let Err(err) = state.permission_service.write_tuple(parent_tuple).await {
        error!("Failed to write parent permission tuple: {}", err);
        // Clean up: delete the channel
        let _ = delete_channel_from_db(&waddle_db, &channel_id).await;
        return channel_error_to_response(ChannelError::Permission(err)).into_response();
    }

    info!(
        "Channel created: {} ({}) in waddle {}",
        request.name, channel_id, waddle_id
    );

    (
        StatusCode::CREATED,
        Json(ChannelResponse {
            id: channel_id,
            waddle_id,
            name: request.name,
            description: request.description,
            channel_type: request.channel_type,
            position: request.position,
            is_default: false,
            created_at: now.clone(),
            updated_at: Some(now),
        }),
    )
        .into_response()
}

/// GET /v1/waddles/:waddle_id/channels
///
/// List all channels in a waddle. Requires view permission on the waddle.
#[instrument(skip(state))]
pub async fn list_channels_handler(
    State(state): State<Arc<ChannelState>>,
    Path(waddle_id): Path<String>,
    Query(params): Query<ListChannelsQuery>,
) -> impl IntoResponse {
    debug!("Listing channels in waddle: {}", waddle_id);

    // Validate session
    let session = match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return channel_error_to_response(ChannelError::Auth(err)).into_response();
        }
    };

    // Check if user has permission to view this waddle
    let subject = Subject::user(&session.did);
    let waddle_object = Object::new(ObjectType::Waddle, &waddle_id);

    let can_view = state
        .permission_service
        .check(&subject, "view", &waddle_object)
        .await
        .map(|r| r.allowed)
        .unwrap_or(false);

    if !can_view {
        return channel_error_to_response(ChannelError::Permission(PermissionError::Denied(
            "You do not have permission to view this waddle".to_string(),
        )))
        .into_response();
    }

    // Get the waddle database
    let waddle_db = match state.app_state.db_pool.get_waddle_db(&waddle_id).await {
        Ok(db) => db,
        Err(err) => {
            error!("Failed to get waddle database: {}", err);
            return channel_error_to_response(ChannelError::Database(format!(
                "Failed to access waddle database: {}",
                err
            )))
            .into_response();
        }
    };

    // List channels from database
    let channels = match list_channels_from_db(&waddle_db, &waddle_id, params.limit, params.offset)
        .await
    {
        Ok(channels) => channels,
        Err(err) => {
            error!("Failed to list channels: {}", err);
            return channel_error_to_response(ChannelError::Database(err)).into_response();
        }
    };

    let total = channels.len();

    (StatusCode::OK, Json(ListChannelsResponse { channels, total })).into_response()
}

/// GET /v1/channels/:id
///
/// Get channel details. Requires view permission on the parent waddle.
#[instrument(skip(state))]
pub async fn get_channel_handler(
    State(state): State<Arc<ChannelState>>,
    Path(channel_id): Path<String>,
    Query(params): Query<ChannelQuery>,
) -> impl IntoResponse {
    debug!("Getting channel: {}", channel_id);

    // Validate session
    let session = match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return channel_error_to_response(ChannelError::Auth(err)).into_response();
        }
    };

    let waddle_id = &params.waddle_id;

    // Get the waddle database first to check if channel exists
    let waddle_db = match state.app_state.db_pool.get_waddle_db(waddle_id).await {
        Ok(db) => db,
        Err(err) => {
            error!("Failed to get waddle database: {}", err);
            return channel_error_to_response(ChannelError::Database(format!(
                "Failed to access waddle database: {}",
                err
            )))
            .into_response();
        }
    };

    // Check if channel exists BEFORE checking permissions
    // This ensures we return 404 for non-existent channels, not 403
    let channel = match get_channel_from_db(&waddle_db, waddle_id, &channel_id).await {
        Ok(Some(channel)) => channel,
        Ok(None) => {
            return channel_error_to_response(ChannelError::NotFound(format!(
                "Channel '{}' not found",
                channel_id
            )))
            .into_response();
        }
        Err(err) => {
            error!("Failed to get channel: {}", err);
            return channel_error_to_response(ChannelError::Database(err)).into_response();
        }
    };

    // Check if user has permission to view this channel (via waddle membership)
    let subject = Subject::user(&session.did);
    let channel_object = Object::new(ObjectType::Channel, &channel_id);

    let can_view = state
        .permission_service
        .check(&subject, "view", &channel_object)
        .await
        .map(|r| r.allowed)
        .unwrap_or(false);

    if !can_view {
        return channel_error_to_response(ChannelError::Permission(PermissionError::Denied(
            "You do not have permission to view this channel".to_string(),
        )))
        .into_response();
    }

    (StatusCode::OK, Json(channel)).into_response()
}

/// PATCH /v1/channels/:id
///
/// Update channel metadata. Requires manage permission on the channel.
#[instrument(skip(state))]
pub async fn update_channel_handler(
    State(state): State<Arc<ChannelState>>,
    Path(channel_id): Path<String>,
    Query(params): Query<ChannelQuery>,
    Json(request): Json<UpdateChannelRequest>,
) -> impl IntoResponse {
    info!("Updating channel: {}", channel_id);

    // Validate session
    let session = match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return channel_error_to_response(ChannelError::Auth(err)).into_response();
        }
    };

    let waddle_id = &params.waddle_id;

    // Check if user has permission to manage this channel
    let subject = Subject::user(&session.did);
    let channel_object = Object::new(ObjectType::Channel, &channel_id);

    let can_manage = state
        .permission_service
        .check(&subject, "manage", &channel_object)
        .await
        .map(|r| r.allowed)
        .unwrap_or(false);

    if !can_manage {
        return channel_error_to_response(ChannelError::Permission(PermissionError::Denied(
            "You do not have permission to manage this channel".to_string(),
        )))
        .into_response();
    }

    // Get the waddle database
    let waddle_db = match state.app_state.db_pool.get_waddle_db(waddle_id).await {
        Ok(db) => db,
        Err(err) => {
            error!("Failed to get waddle database: {}", err);
            return channel_error_to_response(ChannelError::Database(format!(
                "Failed to access waddle database: {}",
                err
            )))
            .into_response();
        }
    };

    // Check if channel exists
    if let Ok(None) = get_channel_from_db(&waddle_db, waddle_id, &channel_id).await {
        return channel_error_to_response(ChannelError::NotFound(format!(
            "Channel '{}' not found",
            channel_id
        )))
        .into_response();
    }

    // Update channel in database
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(err) = update_channel_in_db(
        &waddle_db,
        &channel_id,
        request.name.as_deref(),
        request.description.as_deref(),
        request.position,
        &now,
    )
    .await
    {
        error!("Failed to update channel: {}", err);
        return channel_error_to_response(ChannelError::Database(err)).into_response();
    }

    // Get updated channel
    let updated_channel = match get_channel_from_db(&waddle_db, waddle_id, &channel_id).await {
        Ok(Some(channel)) => channel,
        Ok(None) => {
            return channel_error_to_response(ChannelError::NotFound(format!(
                "Channel '{}' not found after update",
                channel_id
            )))
            .into_response();
        }
        Err(err) => {
            error!("Failed to get updated channel: {}", err);
            return channel_error_to_response(ChannelError::Database(err)).into_response();
        }
    };

    info!("Channel updated: {}", channel_id);

    (StatusCode::OK, Json(updated_channel)).into_response()
}

/// DELETE /v1/channels/:id
///
/// Delete a channel. Requires delete permission (waddle admin only).
#[instrument(skip(state))]
pub async fn delete_channel_handler(
    State(state): State<Arc<ChannelState>>,
    Path(channel_id): Path<String>,
    Query(params): Query<ChannelQuery>,
) -> impl IntoResponse {
    info!("Deleting channel: {}", channel_id);

    // Validate session
    let session = match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return channel_error_to_response(ChannelError::Auth(err)).into_response();
        }
    };

    let waddle_id = &params.waddle_id;

    // Get the waddle database first to check if channel exists
    let waddle_db = match state.app_state.db_pool.get_waddle_db(waddle_id).await {
        Ok(db) => db,
        Err(err) => {
            error!("Failed to get waddle database: {}", err);
            return channel_error_to_response(ChannelError::Database(format!(
                "Failed to access waddle database: {}",
                err
            )))
            .into_response();
        }
    };

    // Check if channel exists
    let channel = match get_channel_from_db(&waddle_db, waddle_id, &channel_id).await {
        Ok(Some(channel)) => channel,
        Ok(None) => {
            return channel_error_to_response(ChannelError::NotFound(format!(
                "Channel '{}' not found",
                channel_id
            )))
            .into_response();
        }
        Err(err) => {
            error!("Failed to get channel: {}", err);
            return channel_error_to_response(ChannelError::Database(err)).into_response();
        }
    };

    // Prevent deletion of default channel
    if channel.is_default {
        return channel_error_to_response(ChannelError::InvalidInput(
            "Cannot delete the default channel".to_string(),
        ))
        .into_response();
    }

    // Check if user has permission to delete this channel
    let subject = Subject::user(&session.did);
    let channel_object = Object::new(ObjectType::Channel, &channel_id);

    let can_delete = state
        .permission_service
        .check(&subject, "delete", &channel_object)
        .await
        .map(|r| r.allowed)
        .unwrap_or(false);

    if !can_delete {
        return channel_error_to_response(ChannelError::Permission(PermissionError::Denied(
            "You do not have permission to delete this channel".to_string(),
        )))
        .into_response();
    }

    // Delete permission tuples for this channel
    let parent_tuple = Tuple::new(
        Object::new(ObjectType::Channel, &channel_id),
        Relation::new("parent"),
        Subject {
            subject_type: SubjectType::Waddle,
            id: waddle_id.to_string(),
            relation: None,
        },
    );

    if let Err(err) = state.permission_service.delete_tuple(&parent_tuple).await {
        warn!("Failed to delete parent permission tuple: {}", err);
        // Continue - the channel should still be deleted
    }

    // Delete channel from database
    if let Err(err) = delete_channel_from_db(&waddle_db, &channel_id).await {
        error!("Failed to delete channel: {}", err);
        return channel_error_to_response(ChannelError::Database(err)).into_response();
    }

    info!("Channel deleted: {}", channel_id);

    StatusCode::NO_CONTENT.into_response()
}

// === Database Helper Functions ===

/// Insert a new channel into the per-waddle database
#[allow(clippy::too_many_arguments)]
async fn insert_channel(
    db: &Database,
    id: &str,
    name: &str,
    description: Option<&str>,
    channel_type: &str,
    position: i32,
    is_default: bool,
    now: &str,
) -> Result<(), String> {
    let query = r#"
        INSERT INTO channels (id, name, description, channel_type, position, is_default, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
    "#;

    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        conn.execute(
            query,
            libsql::params![
                id,
                name,
                description,
                channel_type,
                position,
                is_default as i32,
                now,
                now
            ],
        )
        .await
        .map_err(|e| format!("Failed to insert channel: {}", e))?;
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        conn.execute(
            query,
            libsql::params![
                id,
                name,
                description,
                channel_type,
                position,
                is_default as i32,
                now,
                now
            ],
        )
        .await
        .map_err(|e| format!("Failed to insert channel: {}", e))?;
    }

    Ok(())
}

/// Get a channel from the per-waddle database
async fn get_channel_from_db(
    db: &Database,
    waddle_id: &str,
    channel_id: &str,
) -> Result<Option<ChannelResponse>, String> {
    let query = r#"
        SELECT id, name, description, channel_type, position, is_default, created_at, updated_at
        FROM channels
        WHERE id = ?
    "#;

    let row = if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        let mut rows = conn
            .query(query, libsql::params![channel_id])
            .await
            .map_err(|e| format!("Failed to query channel: {}", e))?;

        rows.next()
            .await
            .map_err(|e| format!("Failed to read channel row: {}", e))?
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        let mut rows = conn
            .query(query, libsql::params![channel_id])
            .await
            .map_err(|e| format!("Failed to query channel: {}", e))?;

        rows.next()
            .await
            .map_err(|e| format!("Failed to read channel row: {}", e))?
    };

    match row {
        Some(row) => {
            let id: String = row
                .get(0)
                .map_err(|e| format!("Failed to get id: {}", e))?;
            let name: String = row
                .get(1)
                .map_err(|e| format!("Failed to get name: {}", e))?;
            let description: Option<String> = row.get(2).ok();
            let channel_type: String = row
                .get(3)
                .map_err(|e| format!("Failed to get channel_type: {}", e))?;
            let position: i32 = row
                .get(4)
                .map_err(|e| format!("Failed to get position: {}", e))?;
            let is_default: i32 = row
                .get(5)
                .map_err(|e| format!("Failed to get is_default: {}", e))?;
            let created_at: String = row
                .get(6)
                .map_err(|e| format!("Failed to get created_at: {}", e))?;
            let updated_at: Option<String> = row.get(7).ok();

            Ok(Some(ChannelResponse {
                id,
                waddle_id: waddle_id.to_string(),
                name,
                description,
                channel_type,
                position,
                is_default: is_default != 0,
                created_at,
                updated_at,
            }))
        }
        None => Ok(None),
    }
}

/// List channels from the per-waddle database
async fn list_channels_from_db(
    db: &Database,
    waddle_id: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<ChannelResponse>, String> {
    let query = r#"
        SELECT id, name, description, channel_type, position, is_default, created_at, updated_at
        FROM channels
        ORDER BY position ASC, created_at ASC
        LIMIT ? OFFSET ?
    "#;

    let mut channels = Vec::new();

    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        let mut rows = conn
            .query(query, libsql::params![limit as i64, offset as i64])
            .await
            .map_err(|e| format!("Failed to query channels: {}", e))?;

        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read channel row: {}", e))?
        {
            let channel = parse_channel_row(&row, waddle_id)?;
            channels.push(channel);
        }
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        let mut rows = conn
            .query(query, libsql::params![limit as i64, offset as i64])
            .await
            .map_err(|e| format!("Failed to query channels: {}", e))?;

        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read channel row: {}", e))?
        {
            let channel = parse_channel_row(&row, waddle_id)?;
            channels.push(channel);
        }
    }

    Ok(channels)
}

/// Parse a channel row from the database
fn parse_channel_row(row: &libsql::Row, waddle_id: &str) -> Result<ChannelResponse, String> {
    let id: String = row
        .get(0)
        .map_err(|e| format!("Failed to get id: {}", e))?;
    let name: String = row
        .get(1)
        .map_err(|e| format!("Failed to get name: {}", e))?;
    let description: Option<String> = row.get(2).ok();
    let channel_type: String = row
        .get(3)
        .map_err(|e| format!("Failed to get channel_type: {}", e))?;
    let position: i32 = row
        .get(4)
        .map_err(|e| format!("Failed to get position: {}", e))?;
    let is_default: i32 = row
        .get(5)
        .map_err(|e| format!("Failed to get is_default: {}", e))?;
    let created_at: String = row
        .get(6)
        .map_err(|e| format!("Failed to get created_at: {}", e))?;
    let updated_at: Option<String> = row.get(7).ok();

    Ok(ChannelResponse {
        id,
        waddle_id: waddle_id.to_string(),
        name,
        description,
        channel_type,
        position,
        is_default: is_default != 0,
        created_at,
        updated_at,
    })
}

/// Update a channel in the per-waddle database
async fn update_channel_in_db(
    db: &Database,
    channel_id: &str,
    name: Option<&str>,
    description: Option<&str>,
    position: Option<i32>,
    now: &str,
) -> Result<(), String> {
    // Build dynamic update query based on provided fields
    let mut updates = vec!["updated_at = ?".to_string()];
    let mut params: Vec<libsql::Value> = vec![now.into()];

    if let Some(name) = name {
        updates.push("name = ?".to_string());
        params.push(name.into());
    }
    if let Some(description) = description {
        updates.push("description = ?".to_string());
        params.push(description.into());
    }
    if let Some(position) = position {
        updates.push("position = ?".to_string());
        params.push(position.into());
    }

    params.push(channel_id.into());

    let query = format!("UPDATE channels SET {} WHERE id = ?", updates.join(", "));

    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        conn.execute(&query, params)
            .await
            .map_err(|e| format!("Failed to update channel: {}", e))?;
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        conn.execute(&query, params)
            .await
            .map_err(|e| format!("Failed to update channel: {}", e))?;
    }

    Ok(())
}

/// Delete a channel from the per-waddle database
async fn delete_channel_from_db(db: &Database, channel_id: &str) -> Result<(), String> {
    // Messages will be cascade deleted due to foreign key constraint
    let query = "DELETE FROM channels WHERE id = ?";

    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        conn.execute(query, libsql::params![channel_id])
            .await
            .map_err(|e| format!("Failed to delete channel: {}", e))?;
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        conn.execute(query, libsql::params![channel_id])
            .await
            .map_err(|e| format!("Failed to delete channel: {}", e))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Session;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use crate::permissions::{Object, ObjectType, Relation, Subject, Tuple};
    use axum::body::Body;
    use axum::http::Request;
    use chrono::{Duration, Utc};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn create_test_channel_state() -> Arc<ChannelState> {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let db_pool = DatabasePool::new(config, pool_config).await.unwrap();

        // Run migrations
        let runner = MigrationRunner::global();
        runner.run(db_pool.global()).await.unwrap();

        let app_state = Arc::new(AppState::new(db_pool, crate::config::ServerConfig::test_homeserver()));
        Arc::new(ChannelState::new(
            app_state,
            Some(b"test-encryption-key-32-bytes!!!"),
        ))
    }

    async fn create_test_session(state: &ChannelState) -> Session {
        let session = Session {
            id: format!("test-session-{}", Uuid::new_v4()),
            did: format!(
                "did:plc:test{}",
                Uuid::new_v4().to_string().replace('-', "")[..16].to_string()
            ),
            handle: "test.bsky.social".to_string(),
            access_token: "test-token".to_string(),
            refresh_token: None,
            token_endpoint: "https://bsky.social/oauth/token".to_string(),
            pds_url: "https://bsky.social".to_string(),
            expires_at: Some(Utc::now() + Duration::hours(1)),
            created_at: Utc::now(),
            last_used_at: Utc::now(),
        };

        state
            .session_manager
            .create_session(&session)
            .await
            .unwrap();

        session
    }

    async fn setup_waddle_with_permissions(
        state: &ChannelState,
        session: &Session,
    ) -> (String, Database) {
        let waddle_id = Uuid::new_v4().to_string();

        // Create owner permission tuple
        let owner_tuple = Tuple::new(
            Object::new(ObjectType::Waddle, &waddle_id),
            Relation::new("owner"),
            Subject::user(&session.did),
        );
        state.permission_service.write_tuple(owner_tuple).await.unwrap();

        // Create admin permission tuple (for delete channel permission)
        let admin_tuple = Tuple::new(
            Object::new(ObjectType::Waddle, &waddle_id),
            Relation::new("admin"),
            Subject::user(&session.did),
        );
        state.permission_service.write_tuple(admin_tuple).await.unwrap();

        // Create member permission tuple (for view permission)
        let member_tuple = Tuple::new(
            Object::new(ObjectType::Waddle, &waddle_id),
            Relation::new("member"),
            Subject::user(&session.did),
        );
        state.permission_service.write_tuple(member_tuple).await.unwrap();

        // Create the waddle database and run migrations
        let waddle_db = state
            .app_state
            .db_pool
            .create_waddle_db(&waddle_id)
            .await
            .unwrap();
        let runner = MigrationRunner::waddle();
        runner.run(&waddle_db).await.unwrap();

        (waddle_id, waddle_db)
    }

    #[tokio::test]
    async fn test_create_channel() {
        let channel_state = create_test_channel_state().await;
        let session = create_test_session(&channel_state).await;
        let (waddle_id, _waddle_db) =
            setup_waddle_with_permissions(&channel_state, &session).await;
        let app = router(channel_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/waddles/{}/channels?session_id={}",
                        waddle_id, session.id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{"name": "test-channel", "description": "A test channel"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["name"], "test-channel");
        assert_eq!(json["description"], "A test channel");
        assert_eq!(json["waddle_id"], waddle_id);
        assert_eq!(json["channel_type"], "text");
        assert!(!json["is_default"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_create_channel_empty_name() {
        let channel_state = create_test_channel_state().await;
        let session = create_test_session(&channel_state).await;
        let (waddle_id, _waddle_db) =
            setup_waddle_with_permissions(&channel_state, &session).await;
        let app = router(channel_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/waddles/{}/channels?session_id={}",
                        waddle_id, session.id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": ""}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "invalid_input");
    }

    #[tokio::test]
    async fn test_create_channel_permission_denied() {
        let channel_state = create_test_channel_state().await;
        let session = create_test_session(&channel_state).await;
        // Don't set up permissions - user has no create_channel permission
        let waddle_id = Uuid::new_v4().to_string();
        let app = router(channel_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/waddles/{}/channels?session_id={}",
                        waddle_id, session.id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "test-channel"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }

    #[tokio::test]
    async fn test_list_channels() {
        let channel_state = create_test_channel_state().await;
        let session = create_test_session(&channel_state).await;
        let (waddle_id, _waddle_db) =
            setup_waddle_with_permissions(&channel_state, &session).await;
        let app = router(channel_state.clone());

        // Create two channels
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/waddles/{}/channels?session_id={}",
                        waddle_id, session.id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "channel-1", "position": 1}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/waddles/{}/channels?session_id={}",
                        waddle_id, session.id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "channel-2", "position": 2}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // List channels
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/v1/waddles/{}/channels?session_id={}",
                        waddle_id, session.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["total"], 2);
        assert_eq!(json["channels"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_get_channel() {
        let channel_state = create_test_channel_state().await;
        let session = create_test_session(&channel_state).await;
        let (waddle_id, _waddle_db) =
            setup_waddle_with_permissions(&channel_state, &session).await;
        let app = router(channel_state.clone());

        // Create a channel
        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/waddles/{}/channels?session_id={}",
                        waddle_id, session.id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "test-channel"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = create_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let create_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let channel_id = create_json["id"].as_str().unwrap();

        // Get the channel
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/v1/channels/{}?session_id={}&waddle_id={}",
                        channel_id, session.id, waddle_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["id"], channel_id);
        assert_eq!(json["name"], "test-channel");
    }

    #[tokio::test]
    async fn test_get_channel_not_found() {
        let channel_state = create_test_channel_state().await;
        let session = create_test_session(&channel_state).await;
        let (waddle_id, _waddle_db) =
            setup_waddle_with_permissions(&channel_state, &session).await;
        let app = router(channel_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/v1/channels/nonexistent?session_id={}&waddle_id={}",
                        session.id, waddle_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_update_channel() {
        let channel_state = create_test_channel_state().await;
        let session = create_test_session(&channel_state).await;
        let (waddle_id, _waddle_db) =
            setup_waddle_with_permissions(&channel_state, &session).await;
        let app = router(channel_state.clone());

        // Create a channel
        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/waddles/{}/channels?session_id={}",
                        waddle_id, session.id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "test-channel"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = create_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let create_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let channel_id = create_json["id"].as_str().unwrap();

        // Update the channel
        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!(
                        "/v1/channels/{}?session_id={}&waddle_id={}",
                        channel_id, session.id, waddle_id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{"name": "updated-channel", "description": "New description"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["name"], "updated-channel");
        assert_eq!(json["description"], "New description");
    }

    #[tokio::test]
    async fn test_delete_channel() {
        let channel_state = create_test_channel_state().await;
        let session = create_test_session(&channel_state).await;
        let (waddle_id, _waddle_db) =
            setup_waddle_with_permissions(&channel_state, &session).await;
        let app = router(channel_state.clone());

        // Create a channel
        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/waddles/{}/channels?session_id={}",
                        waddle_id, session.id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "test-channel"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = create_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let create_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let channel_id = create_json["id"].as_str().unwrap();

        // Delete the channel
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!(
                        "/v1/channels/{}?session_id={}&waddle_id={}",
                        channel_id, session.id, waddle_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify it's gone
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/v1/channels/{}?session_id={}&waddle_id={}",
                        channel_id, session.id, waddle_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_delete_channel_permission_denied() {
        let channel_state = create_test_channel_state().await;
        let owner_session = create_test_session(&channel_state).await;
        let other_session = create_test_session(&channel_state).await;
        let (waddle_id, _waddle_db) =
            setup_waddle_with_permissions(&channel_state, &owner_session).await;

        // Give other_session member permission only (not admin, so can't delete)
        let member_tuple = Tuple::new(
            Object::new(ObjectType::Waddle, &waddle_id),
            Relation::new("member"),
            Subject::user(&other_session.did),
        );
        channel_state
            .permission_service
            .write_tuple(member_tuple)
            .await
            .unwrap();

        let app = router(channel_state.clone());

        // Create a channel as owner
        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/waddles/{}/channels?session_id={}",
                        waddle_id, owner_session.id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "owner-channel"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = create_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let create_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let channel_id = create_json["id"].as_str().unwrap();

        // Try to delete as other user (non-admin)
        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!(
                        "/v1/channels/{}?session_id={}&waddle_id={}",
                        channel_id, other_session.id, waddle_id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
