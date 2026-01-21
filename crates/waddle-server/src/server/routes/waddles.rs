//! Waddle CRUD API Routes
//!
//! Provides HTTP endpoints for managing Waddles (communities):
//! - POST /v1/waddles - Create a new waddle
//! - GET /v1/waddles - List user's waddles
//! - GET /v1/waddles/:id - Get waddle details
//! - PATCH /v1/waddles/:id - Update waddle metadata
//! - DELETE /v1/waddles/:id - Delete a waddle
//!
//! Member management endpoints:
//! - GET /v1/waddles/:id/members - List waddle members
//! - POST /v1/waddles/:id/members - Add a member to the waddle
//! - DELETE /v1/waddles/:id/members/:did - Remove a member from the waddle

use crate::auth::{AuthError, SessionManager};
use crate::db::{Database, MigrationRunner};
use crate::permissions::{
    Object, ObjectType, PermissionError, PermissionService, Relation, Subject, Tuple,
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

/// Extended application state for waddle routes
pub struct WaddleState {
    /// Core app state
    pub app_state: Arc<AppState>,
    /// Permission service
    pub permission_service: PermissionService,
    /// Session manager
    pub session_manager: SessionManager,
}

impl WaddleState {
    /// Create new waddle state
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

/// Create the waddles router
pub fn router(waddle_state: Arc<WaddleState>) -> Router {
    Router::new()
        .route("/v1/waddles", post(create_waddle_handler))
        .route("/v1/waddles", get(list_waddles_handler))
        .route("/v1/waddles/:id", get(get_waddle_handler))
        .route("/v1/waddles/:id", patch(update_waddle_handler))
        .route("/v1/waddles/:id", delete(delete_waddle_handler))
        // Member management routes
        .route("/v1/waddles/:id/members", get(list_members_handler))
        .route("/v1/waddles/:id/members", post(add_member_handler))
        .route(
            "/v1/waddles/:id/members/:member_did",
            delete(remove_member_handler),
        )
        .with_state(waddle_state)
}

// === Request/Response Types ===

/// Request body for creating a new waddle
#[derive(Debug, Deserialize)]
pub struct CreateWaddleRequest {
    /// Waddle name (required)
    pub name: String,
    /// Waddle description (optional)
    pub description: Option<String>,
    /// Icon URL (optional)
    pub icon_url: Option<String>,
    /// Whether the waddle is public (default: true)
    #[serde(default = "default_is_public")]
    pub is_public: bool,
}

fn default_is_public() -> bool {
    true
}

/// Request body for updating a waddle
#[derive(Debug, Deserialize)]
pub struct UpdateWaddleRequest {
    /// New waddle name (optional)
    pub name: Option<String>,
    /// New description (optional)
    pub description: Option<String>,
    /// New icon URL (optional)
    pub icon_url: Option<String>,
    /// New public status (optional)
    pub is_public: Option<bool>,
}

/// Response for a single waddle
#[derive(Debug, Serialize)]
pub struct WaddleResponse {
    /// Waddle ID
    pub id: String,
    /// Waddle name
    pub name: String,
    /// Waddle description
    pub description: Option<String>,
    /// Owner's DID
    pub owner_did: String,
    /// Icon URL
    pub icon_url: Option<String>,
    /// Whether the waddle is public
    pub is_public: bool,
    /// User's role in this waddle (if applicable)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    /// When the waddle was created
    pub created_at: String,
    /// When the waddle was last updated
    pub updated_at: Option<String>,
}

/// Response for list of waddles
#[derive(Debug, Serialize)]
pub struct ListWaddlesResponse {
    /// List of waddles
    pub waddles: Vec<WaddleResponse>,
    /// Total count
    pub total: usize,
}

/// Query parameters for session authentication
#[derive(Debug, Deserialize)]
pub struct SessionQuery {
    /// Session ID for authentication
    pub session_id: String,
}

/// Query parameters for listing waddles
#[derive(Debug, Deserialize)]
pub struct ListWaddlesQuery {
    /// Session ID for authentication
    pub session_id: String,
    /// Maximum number of results (default: 50)
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Offset for pagination (default: 0)
    #[serde(default)]
    pub offset: usize,
}

fn default_limit() -> usize {
    50
}

// === Member Management Request/Response Types ===

/// Request body for adding a member to a waddle
#[derive(Debug, Deserialize)]
pub struct AddMemberRequest {
    /// DID of the user to add as a member
    pub did: String,
    /// Role for the new member (default: "member")
    #[serde(default = "default_member_role")]
    pub role: String,
}

fn default_member_role() -> String {
    "member".to_string()
}

/// Response for a single waddle member
#[derive(Debug, Serialize)]
pub struct MemberResponse {
    /// User's DID
    pub did: String,
    /// User's handle
    pub handle: String,
    /// User's role in the waddle (owner, admin, moderator, member)
    pub role: String,
    /// When the user joined the waddle
    pub joined_at: String,
}

/// Response for list of waddle members
#[derive(Debug, Serialize)]
pub struct ListMembersResponse {
    /// List of members
    pub members: Vec<MemberResponse>,
    /// Total count
    pub total: usize,
}

/// Query parameters for listing waddle members
#[derive(Debug, Deserialize)]
pub struct ListMembersQuery {
    /// Session ID for authentication
    pub session_id: String,
    /// Maximum number of results (default: 50)
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Offset for pagination (default: 0)
    #[serde(default)]
    pub offset: usize,
}

/// Path parameters for member removal
#[derive(Debug, Deserialize)]
pub struct MemberPath {
    /// Waddle ID
    pub id: String,
    /// Member DID (URL-encoded)
    pub member_did: String,
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

/// Waddle-specific error type
#[derive(Debug)]
pub enum WaddleError {
    Auth(AuthError),
    Permission(PermissionError),
    NotFound(String),
    Database(String),
    InvalidInput(String),
}

impl From<AuthError> for WaddleError {
    fn from(err: AuthError) -> Self {
        WaddleError::Auth(err)
    }
}

impl From<PermissionError> for WaddleError {
    fn from(err: PermissionError) -> Self {
        WaddleError::Permission(err)
    }
}

/// Convert WaddleError to HTTP response
fn waddle_error_to_response(err: WaddleError) -> (StatusCode, Json<ErrorResponse>) {
    match err {
        WaddleError::Auth(auth_err) => {
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
        WaddleError::Permission(perm_err) => {
            let (status, error_code) = match &perm_err {
                PermissionError::Denied(_) => (StatusCode::FORBIDDEN, "permission_denied"),
                _ => (StatusCode::BAD_REQUEST, "permission_error"),
            };
            (
                status,
                Json(ErrorResponse::new(error_code, &perm_err.to_string())),
            )
        }
        WaddleError::NotFound(msg) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::new("not_found", &msg)),
        ),
        WaddleError::Database(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new("database_error", &msg)),
        ),
        WaddleError::InvalidInput(msg) => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new("invalid_input", &msg)),
        ),
    }
}

// === Handlers ===

/// POST /v1/waddles
///
/// Create a new waddle with the authenticated user as owner.
#[instrument(skip(state))]
pub async fn create_waddle_handler(
    State(state): State<Arc<WaddleState>>,
    Query(params): Query<SessionQuery>,
    Json(request): Json<CreateWaddleRequest>,
) -> impl IntoResponse {
    info!("Creating waddle: {}", request.name);

    // Validate session
    let session = match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return waddle_error_to_response(WaddleError::Auth(err)).into_response();
        }
    };

    // Validate input
    if request.name.trim().is_empty() {
        return waddle_error_to_response(WaddleError::InvalidInput(
            "Waddle name cannot be empty".to_string(),
        ))
        .into_response();
    }

    // Generate waddle ID
    let waddle_id = Uuid::new_v4().to_string();

    // Get user ID from database
    let user_id = match get_user_id(state.app_state.db_pool.global(), &session.did).await {
        Ok(id) => id,
        Err(err) => {
            error!("Failed to get user ID: {}", err);
            return waddle_error_to_response(WaddleError::Database(err)).into_response();
        }
    };

    // Insert waddle into database
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(err) = insert_waddle(
        state.app_state.db_pool.global(),
        &waddle_id,
        &request.name,
        request.description.as_deref(),
        user_id,
        request.icon_url.as_deref(),
        request.is_public,
        &now,
    )
    .await
    {
        error!("Failed to insert waddle: {}", err);
        return waddle_error_to_response(WaddleError::Database(err)).into_response();
    }

    // Add owner as waddle member with owner role
    if let Err(err) =
        add_waddle_member(state.app_state.db_pool.global(), &waddle_id, user_id, "owner").await
    {
        error!("Failed to add owner as member: {}", err);
        // Clean up: delete the waddle
        let _ = delete_waddle_from_db(state.app_state.db_pool.global(), &waddle_id).await;
        return waddle_error_to_response(WaddleError::Database(err)).into_response();
    }

    // Create owner permission tuple
    let owner_tuple = Tuple::new(
        Object::new(ObjectType::Waddle, &waddle_id),
        Relation::new("owner"),
        Subject::user(&session.did),
    );

    if let Err(err) = state.permission_service.write_tuple(owner_tuple).await {
        error!("Failed to write owner permission tuple: {}", err);
        // Clean up: delete the waddle
        let _ = delete_waddle_from_db(state.app_state.db_pool.global(), &waddle_id).await;
        return waddle_error_to_response(WaddleError::Permission(err)).into_response();
    }

    // Create per-waddle database and default channels
    match state.app_state.db_pool.create_waddle_db(&waddle_id).await {
        Ok(waddle_db) => {
            // Run waddle migrations
            let runner = MigrationRunner::waddle();
            if let Err(err) = runner.run(&waddle_db).await {
                warn!("Failed to run waddle migrations: {}", err);
                // Continue - the database was created, migrations can be retried
            }

            // Create default #general channel
            if let Err(err) = create_default_channel(&waddle_db).await {
                warn!("Failed to create default channel: {}", err);
                // Continue - the waddle was created successfully
            }
        }
        Err(err) => {
            warn!("Failed to create waddle database: {}", err);
            // Continue - the waddle was created, DB can be created lazily
        }
    }

    info!("Waddle created: {} ({})", request.name, waddle_id);

    (
        StatusCode::CREATED,
        Json(WaddleResponse {
            id: waddle_id,
            name: request.name,
            description: request.description,
            owner_did: session.did,
            icon_url: request.icon_url,
            is_public: request.is_public,
            role: Some("owner".to_string()),
            created_at: now.clone(),
            updated_at: Some(now),
        }),
    )
        .into_response()
}

/// GET /v1/waddles/:id
///
/// Get waddle details with permission check.
#[instrument(skip(state))]
pub async fn get_waddle_handler(
    State(state): State<Arc<WaddleState>>,
    Path(waddle_id): Path<String>,
    Query(params): Query<SessionQuery>,
) -> impl IntoResponse {
    debug!("Getting waddle: {}", waddle_id);

    // Validate session
    let session = match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return waddle_error_to_response(WaddleError::Auth(err)).into_response();
        }
    };

    // Get waddle from database
    let waddle = match get_waddle_from_db(state.app_state.db_pool.global(), &waddle_id).await {
        Ok(Some(waddle)) => waddle,
        Ok(None) => {
            return waddle_error_to_response(WaddleError::NotFound(format!(
                "Waddle '{}' not found",
                waddle_id
            )))
            .into_response();
        }
        Err(err) => {
            error!("Failed to get waddle: {}", err);
            return waddle_error_to_response(WaddleError::Database(err)).into_response();
        }
    };

    // Check if user has permission to view this waddle
    // Either the waddle is public OR user is a member
    let subject = Subject::user(&session.did);
    let object = Object::new(ObjectType::Waddle, &waddle_id);

    let has_view_permission = waddle.is_public
        || state
            .permission_service
            .check(&subject, "view", &object)
            .await
            .map(|r| r.allowed)
            .unwrap_or(false);

    if !has_view_permission {
        return waddle_error_to_response(WaddleError::Permission(PermissionError::Denied(
            "You do not have permission to view this waddle".to_string(),
        )))
        .into_response();
    }

    // Get user's role in this waddle
    let role = get_user_role(state.app_state.db_pool.global(), &waddle_id, &session.did)
        .await
        .ok()
        .flatten();

    (StatusCode::OK, Json(WaddleResponse { role, ..waddle })).into_response()
}

/// PATCH /v1/waddles/:id
///
/// Update waddle metadata with owner/admin permission check.
#[instrument(skip(state))]
pub async fn update_waddle_handler(
    State(state): State<Arc<WaddleState>>,
    Path(waddle_id): Path<String>,
    Query(params): Query<SessionQuery>,
    Json(request): Json<UpdateWaddleRequest>,
) -> impl IntoResponse {
    info!("Updating waddle: {}", waddle_id);

    // Validate session
    let session = match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return waddle_error_to_response(WaddleError::Auth(err)).into_response();
        }
    };

    // Check if waddle exists
    let _waddle = match get_waddle_from_db(state.app_state.db_pool.global(), &waddle_id).await {
        Ok(Some(waddle)) => waddle,
        Ok(None) => {
            return waddle_error_to_response(WaddleError::NotFound(format!(
                "Waddle '{}' not found",
                waddle_id
            )))
            .into_response();
        }
        Err(err) => {
            error!("Failed to get waddle: {}", err);
            return waddle_error_to_response(WaddleError::Database(err)).into_response();
        }
    };

    // Check if user has permission to update this waddle (owner or admin)
    let subject = Subject::user(&session.did);
    let object = Object::new(ObjectType::Waddle, &waddle_id);

    let can_update = state
        .permission_service
        .check(&subject, "update", &object)
        .await
        .map(|r| r.allowed)
        .unwrap_or(false);

    if !can_update {
        return waddle_error_to_response(WaddleError::Permission(PermissionError::Denied(
            "You do not have permission to update this waddle".to_string(),
        )))
        .into_response();
    }

    // Update waddle in database
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(err) = update_waddle_in_db(
        state.app_state.db_pool.global(),
        &waddle_id,
        request.name.as_deref(),
        request.description.as_deref(),
        request.icon_url.as_deref(),
        request.is_public,
        &now,
    )
    .await
    {
        error!("Failed to update waddle: {}", err);
        return waddle_error_to_response(WaddleError::Database(err)).into_response();
    }

    // Get updated waddle
    let updated_waddle =
        match get_waddle_from_db(state.app_state.db_pool.global(), &waddle_id).await {
            Ok(Some(waddle)) => waddle,
            Ok(None) => {
                return waddle_error_to_response(WaddleError::NotFound(format!(
                    "Waddle '{}' not found after update",
                    waddle_id
                )))
                .into_response();
            }
            Err(err) => {
                error!("Failed to get updated waddle: {}", err);
                return waddle_error_to_response(WaddleError::Database(err)).into_response();
            }
        };

    // Get user's role
    let role = get_user_role(state.app_state.db_pool.global(), &waddle_id, &session.did)
        .await
        .ok()
        .flatten();

    info!("Waddle updated: {}", waddle_id);

    (
        StatusCode::OK,
        Json(WaddleResponse {
            role,
            ..updated_waddle
        }),
    )
        .into_response()
}

/// DELETE /v1/waddles/:id
///
/// Delete a waddle with owner-only permission check.
#[instrument(skip(state))]
pub async fn delete_waddle_handler(
    State(state): State<Arc<WaddleState>>,
    Path(waddle_id): Path<String>,
    Query(params): Query<SessionQuery>,
) -> impl IntoResponse {
    info!("Deleting waddle: {}", waddle_id);

    // Validate session
    let session = match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return waddle_error_to_response(WaddleError::Auth(err)).into_response();
        }
    };

    // Check if waddle exists
    if let Ok(None) = get_waddle_from_db(state.app_state.db_pool.global(), &waddle_id).await {
        return waddle_error_to_response(WaddleError::NotFound(format!(
            "Waddle '{}' not found",
            waddle_id
        )))
        .into_response();
    }

    // Check if user has permission to delete this waddle (owner only)
    let subject = Subject::user(&session.did);
    let object = Object::new(ObjectType::Waddle, &waddle_id);

    let can_delete = state
        .permission_service
        .check(&subject, "delete", &object)
        .await
        .map(|r| r.allowed)
        .unwrap_or(false);

    if !can_delete {
        return waddle_error_to_response(WaddleError::Permission(PermissionError::Denied(
            "Only the owner can delete this waddle".to_string(),
        )))
        .into_response();
    }

    // Delete permission tuples for this waddle
    // Note: In a full implementation, we'd delete all tuples related to this waddle
    let owner_tuple = Tuple::new(
        Object::new(ObjectType::Waddle, &waddle_id),
        Relation::new("owner"),
        Subject::user(&session.did),
    );

    if let Err(err) = state.permission_service.delete_tuple(&owner_tuple).await {
        warn!("Failed to delete owner permission tuple: {}", err);
        // Continue - the waddle should still be deleted
    }

    // Delete waddle from database (this also cascades to waddle_members)
    if let Err(err) = delete_waddle_from_db(state.app_state.db_pool.global(), &waddle_id).await {
        error!("Failed to delete waddle: {}", err);
        return waddle_error_to_response(WaddleError::Database(err)).into_response();
    }

    // Unload waddle database from pool
    state.app_state.db_pool.unload_waddle_db(&waddle_id);

    info!("Waddle deleted: {}", waddle_id);

    StatusCode::NO_CONTENT.into_response()
}

/// GET /v1/waddles
///
/// List waddles the authenticated user is a member of.
#[instrument(skip(state))]
pub async fn list_waddles_handler(
    State(state): State<Arc<WaddleState>>,
    Query(params): Query<ListWaddlesQuery>,
) -> impl IntoResponse {
    debug!("Listing waddles for session: {}", params.session_id);

    // Validate session
    let session = match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return waddle_error_to_response(WaddleError::Auth(err)).into_response();
        }
    };

    // Get user's waddles from database
    let waddles = match list_user_waddles(
        state.app_state.db_pool.global(),
        &session.did,
        params.limit,
        params.offset,
    )
    .await
    {
        Ok(waddles) => waddles,
        Err(err) => {
            error!("Failed to list waddles: {}", err);
            return waddle_error_to_response(WaddleError::Database(err)).into_response();
        }
    };

    let total = waddles.len();

    (
        StatusCode::OK,
        Json(ListWaddlesResponse { waddles, total }),
    )
        .into_response()
}

// === Member Management Handlers ===

/// GET /v1/waddles/:id/members
///
/// List all members of a waddle with pagination.
#[instrument(skip(state))]
pub async fn list_members_handler(
    State(state): State<Arc<WaddleState>>,
    Path(waddle_id): Path<String>,
    Query(params): Query<ListMembersQuery>,
) -> impl IntoResponse {
    debug!("Listing members for waddle: {}", waddle_id);

    // Validate session
    let session = match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return waddle_error_to_response(WaddleError::Auth(err)).into_response();
        }
    };

    // Check if waddle exists
    if let Ok(None) = get_waddle_from_db(state.app_state.db_pool.global(), &waddle_id).await {
        return waddle_error_to_response(WaddleError::NotFound(format!(
            "Waddle '{}' not found",
            waddle_id
        )))
        .into_response();
    }

    // Check if user has permission to view this waddle
    let subject = Subject::user(&session.did);
    let object = Object::new(ObjectType::Waddle, &waddle_id);

    let can_view = state
        .permission_service
        .check(&subject, "view", &object)
        .await
        .map(|r| r.allowed)
        .unwrap_or(false);

    if !can_view {
        return waddle_error_to_response(WaddleError::Permission(PermissionError::Denied(
            "You do not have permission to view this waddle".to_string(),
        )))
        .into_response();
    }

    // Get members from database
    let members = match list_waddle_members(
        state.app_state.db_pool.global(),
        &waddle_id,
        params.limit,
        params.offset,
    )
    .await
    {
        Ok(members) => members,
        Err(err) => {
            error!("Failed to list members: {}", err);
            return waddle_error_to_response(WaddleError::Database(err)).into_response();
        }
    };

    let total = members.len();

    (
        StatusCode::OK,
        Json(ListMembersResponse { members, total }),
    )
        .into_response()
}

/// POST /v1/waddles/:id/members
///
/// Add a new member to a waddle. Requires manage_members permission (owner/admin/moderator).
#[instrument(skip(state))]
pub async fn add_member_handler(
    State(state): State<Arc<WaddleState>>,
    Path(waddle_id): Path<String>,
    Query(params): Query<SessionQuery>,
    Json(request): Json<AddMemberRequest>,
) -> impl IntoResponse {
    info!("Adding member {} to waddle {}", request.did, waddle_id);

    // Validate session
    let session = match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return waddle_error_to_response(WaddleError::Auth(err)).into_response();
        }
    };

    // Validate input - check that DID is not empty
    if request.did.trim().is_empty() {
        return waddle_error_to_response(WaddleError::InvalidInput(
            "DID cannot be empty".to_string(),
        ))
        .into_response();
    }

    // Validate role
    let valid_roles = ["member", "moderator", "admin"];
    if !valid_roles.contains(&request.role.as_str()) {
        return waddle_error_to_response(WaddleError::InvalidInput(format!(
            "Invalid role '{}'. Valid roles are: member, moderator, admin",
            request.role
        )))
        .into_response();
    }

    // Check if waddle exists
    if let Ok(None) = get_waddle_from_db(state.app_state.db_pool.global(), &waddle_id).await {
        return waddle_error_to_response(WaddleError::NotFound(format!(
            "Waddle '{}' not found",
            waddle_id
        )))
        .into_response();
    }

    // Check if user has permission to manage members
    let subject = Subject::user(&session.did);
    let object = Object::new(ObjectType::Waddle, &waddle_id);

    let can_manage = state
        .permission_service
        .check(&subject, "manage_members", &object)
        .await
        .map(|r| r.allowed)
        .unwrap_or(false);

    if !can_manage {
        return waddle_error_to_response(WaddleError::Permission(PermissionError::Denied(
            "You do not have permission to manage members in this waddle".to_string(),
        )))
        .into_response();
    }

    // Check if user being added exists and get their info
    let new_member_user_id = match get_or_create_user_by_did(
        state.app_state.db_pool.global(),
        &request.did,
    )
    .await
    {
        Ok(user_id) => user_id,
        Err(err) => {
            error!("Failed to get/create user: {}", err);
            return waddle_error_to_response(WaddleError::Database(err)).into_response();
        }
    };

    // Check if member already exists in waddle
    if let Ok(Some(_)) = get_member_role(
        state.app_state.db_pool.global(),
        &waddle_id,
        &request.did,
    )
    .await
    {
        return waddle_error_to_response(WaddleError::InvalidInput(
            "User is already a member of this waddle".to_string(),
        ))
        .into_response();
    }

    // Add member to waddle_members table
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(err) = add_waddle_member_with_timestamp(
        state.app_state.db_pool.global(),
        &waddle_id,
        new_member_user_id,
        &request.role,
        &now,
    )
    .await
    {
        error!("Failed to add member: {}", err);
        return waddle_error_to_response(WaddleError::Database(err)).into_response();
    }

    // Create permission tuple for the new member
    let member_tuple = Tuple::new(
        Object::new(ObjectType::Waddle, &waddle_id),
        Relation::new(&request.role),
        Subject::user(&request.did),
    );

    if let Err(err) = state.permission_service.write_tuple(member_tuple).await {
        error!("Failed to write member permission tuple: {}", err);
        // Clean up: remove the member from the database
        let _ = remove_waddle_member(state.app_state.db_pool.global(), &waddle_id, &request.did).await;
        return waddle_error_to_response(WaddleError::Permission(err)).into_response();
    }

    // Get user handle for response
    let handle = get_user_handle(state.app_state.db_pool.global(), &request.did)
        .await
        .unwrap_or_else(|_| request.did.clone());

    info!("Member {} added to waddle {}", request.did, waddle_id);

    (
        StatusCode::CREATED,
        Json(MemberResponse {
            did: request.did,
            handle,
            role: request.role,
            joined_at: now,
        }),
    )
        .into_response()
}

/// DELETE /v1/waddles/:id/members/:member_did
///
/// Remove a member from a waddle. Requires manage_members permission (owner/admin/moderator).
/// The owner cannot be removed from the waddle.
#[instrument(skip(state))]
pub async fn remove_member_handler(
    State(state): State<Arc<WaddleState>>,
    Path(path): Path<MemberPath>,
    Query(params): Query<SessionQuery>,
) -> impl IntoResponse {
    // URL-decode the member DID
    let member_did = match urlencoding::decode(&path.member_did) {
        Ok(decoded) => decoded.into_owned(),
        Err(_) => path.member_did.clone(),
    };

    info!("Removing member {} from waddle {}", member_did, path.id);

    // Validate session
    let session = match state.session_manager.validate_session(&params.session_id).await {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return waddle_error_to_response(WaddleError::Auth(err)).into_response();
        }
    };

    // Check if waddle exists and get owner info
    let waddle = match get_waddle_from_db(state.app_state.db_pool.global(), &path.id).await {
        Ok(Some(waddle)) => waddle,
        Ok(None) => {
            return waddle_error_to_response(WaddleError::NotFound(format!(
                "Waddle '{}' not found",
                path.id
            )))
            .into_response();
        }
        Err(err) => {
            error!("Failed to get waddle: {}", err);
            return waddle_error_to_response(WaddleError::Database(err)).into_response();
        }
    };

    // Prevent removing the owner
    if member_did == waddle.owner_did {
        return waddle_error_to_response(WaddleError::InvalidInput(
            "Cannot remove the owner from the waddle".to_string(),
        ))
        .into_response();
    }

    // Check if user has permission to manage members
    let subject = Subject::user(&session.did);
    let object = Object::new(ObjectType::Waddle, &path.id);

    let can_manage = state
        .permission_service
        .check(&subject, "manage_members", &object)
        .await
        .map(|r| r.allowed)
        .unwrap_or(false);

    if !can_manage {
        return waddle_error_to_response(WaddleError::Permission(PermissionError::Denied(
            "You do not have permission to manage members in this waddle".to_string(),
        )))
        .into_response();
    }

    // Get the member's current role to delete the correct permission tuple
    let member_role = match get_member_role(
        state.app_state.db_pool.global(),
        &path.id,
        &member_did,
    )
    .await
    {
        Ok(Some(role)) => role,
        Ok(None) => {
            return waddle_error_to_response(WaddleError::NotFound(format!(
                "Member '{}' not found in waddle",
                member_did
            )))
            .into_response();
        }
        Err(err) => {
            error!("Failed to get member role: {}", err);
            return waddle_error_to_response(WaddleError::Database(err)).into_response();
        }
    };

    // Delete permission tuple for the member
    let member_tuple = Tuple::new(
        Object::new(ObjectType::Waddle, &path.id),
        Relation::new(&member_role),
        Subject::user(&member_did),
    );

    if let Err(err) = state.permission_service.delete_tuple(&member_tuple).await {
        warn!("Failed to delete member permission tuple: {}", err);
        // Continue - we still want to remove from database
    }

    // Remove member from waddle_members table
    if let Err(err) = remove_waddle_member(state.app_state.db_pool.global(), &path.id, &member_did).await {
        error!("Failed to remove member: {}", err);
        return waddle_error_to_response(WaddleError::Database(err)).into_response();
    }

    info!("Member {} removed from waddle {}", member_did, path.id);

    StatusCode::NO_CONTENT.into_response()
}

// === Database Helper Functions ===

/// Get user ID by DID
async fn get_user_id(db: &Database, did: &str) -> Result<i64, String> {
    let query = "SELECT id FROM users WHERE did = ?";

    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        let mut rows = conn
            .query(query, libsql::params![did])
            .await
            .map_err(|e| format!("Failed to query user: {}", e))?;

        let row = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read user row: {}", e))?
            .ok_or_else(|| "User not found".to_string())?;

        row.get(0)
            .map_err(|e| format!("Failed to get user id: {}", e))
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        let mut rows = conn
            .query(query, libsql::params![did])
            .await
            .map_err(|e| format!("Failed to query user: {}", e))?;

        let row = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read user row: {}", e))?
            .ok_or_else(|| "User not found".to_string())?;

        row.get(0)
            .map_err(|e| format!("Failed to get user id: {}", e))
    }
}

/// Insert a new waddle into the database
#[allow(clippy::too_many_arguments)]
async fn insert_waddle(
    db: &Database,
    id: &str,
    name: &str,
    description: Option<&str>,
    owner_id: i64,
    icon_url: Option<&str>,
    is_public: bool,
    now: &str,
) -> Result<(), String> {
    let query = r#"
        INSERT INTO waddles (id, name, description, owner_id, icon_url, is_public, created_at, updated_at)
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
                owner_id,
                icon_url,
                is_public as i32,
                now,
                now
            ],
        )
        .await
        .map_err(|e| format!("Failed to insert waddle: {}", e))?;
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
                owner_id,
                icon_url,
                is_public as i32,
                now,
                now
            ],
        )
        .await
        .map_err(|e| format!("Failed to insert waddle: {}", e))?;
    }

    Ok(())
}

/// Add a member to a waddle
async fn add_waddle_member(
    db: &Database,
    waddle_id: &str,
    user_id: i64,
    role: &str,
) -> Result<(), String> {
    let query = r#"
        INSERT INTO waddle_members (waddle_id, user_id, role)
        VALUES (?, ?, ?)
    "#;

    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        conn.execute(query, libsql::params![waddle_id, user_id, role])
            .await
            .map_err(|e| format!("Failed to add waddle member: {}", e))?;
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        conn.execute(query, libsql::params![waddle_id, user_id, role])
            .await
            .map_err(|e| format!("Failed to add waddle member: {}", e))?;
    }

    Ok(())
}

/// Get a waddle from the database
async fn get_waddle_from_db(db: &Database, waddle_id: &str) -> Result<Option<WaddleResponse>, String> {
    let query = r#"
        SELECT w.id, w.name, w.description, u.did as owner_did, w.icon_url, w.is_public, w.created_at, w.updated_at
        FROM waddles w
        JOIN users u ON w.owner_id = u.id
        WHERE w.id = ?
    "#;

    let row = if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        let mut rows = conn
            .query(query, libsql::params![waddle_id])
            .await
            .map_err(|e| format!("Failed to query waddle: {}", e))?;

        rows.next()
            .await
            .map_err(|e| format!("Failed to read waddle row: {}", e))?
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        let mut rows = conn
            .query(query, libsql::params![waddle_id])
            .await
            .map_err(|e| format!("Failed to query waddle: {}", e))?;

        rows.next()
            .await
            .map_err(|e| format!("Failed to read waddle row: {}", e))?
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
            let owner_did: String = row
                .get(3)
                .map_err(|e| format!("Failed to get owner_did: {}", e))?;
            let icon_url: Option<String> = row.get(4).ok();
            let is_public: i32 = row
                .get(5)
                .map_err(|e| format!("Failed to get is_public: {}", e))?;
            let created_at: String = row
                .get(6)
                .map_err(|e| format!("Failed to get created_at: {}", e))?;
            let updated_at: Option<String> = row.get(7).ok();

            Ok(Some(WaddleResponse {
                id,
                name,
                description,
                owner_did,
                icon_url,
                is_public: is_public != 0,
                role: None,
                created_at,
                updated_at,
            }))
        }
        None => Ok(None),
    }
}

/// Get user's role in a waddle
async fn get_user_role(
    db: &Database,
    waddle_id: &str,
    user_did: &str,
) -> Result<Option<String>, String> {
    let query = r#"
        SELECT wm.role
        FROM waddle_members wm
        JOIN users u ON wm.user_id = u.id
        WHERE wm.waddle_id = ? AND u.did = ?
    "#;

    let row = if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        let mut rows = conn
            .query(query, libsql::params![waddle_id, user_did])
            .await
            .map_err(|e| format!("Failed to query role: {}", e))?;

        rows.next()
            .await
            .map_err(|e| format!("Failed to read role row: {}", e))?
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        let mut rows = conn
            .query(query, libsql::params![waddle_id, user_did])
            .await
            .map_err(|e| format!("Failed to query role: {}", e))?;

        rows.next()
            .await
            .map_err(|e| format!("Failed to read role row: {}", e))?
    };

    match row {
        Some(row) => {
            let role: String = row
                .get(0)
                .map_err(|e| format!("Failed to get role: {}", e))?;
            Ok(Some(role))
        }
        None => Ok(None),
    }
}

/// Update a waddle in the database
async fn update_waddle_in_db(
    db: &Database,
    waddle_id: &str,
    name: Option<&str>,
    description: Option<&str>,
    icon_url: Option<&str>,
    is_public: Option<bool>,
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
    if let Some(icon_url) = icon_url {
        updates.push("icon_url = ?".to_string());
        params.push(icon_url.into());
    }
    if let Some(is_public) = is_public {
        updates.push("is_public = ?".to_string());
        params.push((is_public as i32).into());
    }

    params.push(waddle_id.into());

    let query = format!(
        "UPDATE waddles SET {} WHERE id = ?",
        updates.join(", ")
    );

    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        conn.execute(&query, params)
            .await
            .map_err(|e| format!("Failed to update waddle: {}", e))?;
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        conn.execute(&query, params)
            .await
            .map_err(|e| format!("Failed to update waddle: {}", e))?;
    }

    Ok(())
}

/// Delete a waddle from the database
async fn delete_waddle_from_db(db: &Database, waddle_id: &str) -> Result<(), String> {
    // First delete from waddle_members (foreign key constraint)
    let delete_members_query = "DELETE FROM waddle_members WHERE waddle_id = ?";
    let delete_waddle_query = "DELETE FROM waddles WHERE id = ?";

    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        conn.execute(delete_members_query, libsql::params![waddle_id])
            .await
            .map_err(|e| format!("Failed to delete waddle members: {}", e))?;
        conn.execute(delete_waddle_query, libsql::params![waddle_id])
            .await
            .map_err(|e| format!("Failed to delete waddle: {}", e))?;
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        conn.execute(delete_members_query, libsql::params![waddle_id])
            .await
            .map_err(|e| format!("Failed to delete waddle members: {}", e))?;
        conn.execute(delete_waddle_query, libsql::params![waddle_id])
            .await
            .map_err(|e| format!("Failed to delete waddle: {}", e))?;
    }

    Ok(())
}

/// List waddles the user is a member of
async fn list_user_waddles(
    db: &Database,
    user_did: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<WaddleResponse>, String> {
    let query = r#"
        SELECT w.id, w.name, w.description, u.did as owner_did, w.icon_url, w.is_public, w.created_at, w.updated_at, wm.role
        FROM waddles w
        JOIN users u ON w.owner_id = u.id
        JOIN waddle_members wm ON w.id = wm.waddle_id
        JOIN users mu ON wm.user_id = mu.id
        WHERE mu.did = ?
        ORDER BY w.created_at DESC
        LIMIT ? OFFSET ?
    "#;

    let mut waddles = Vec::new();

    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        let mut rows = conn
            .query(query, libsql::params![user_did, limit as i64, offset as i64])
            .await
            .map_err(|e| format!("Failed to query waddles: {}", e))?;

        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read waddle row: {}", e))?
        {
            let waddle = parse_waddle_row(&row)?;
            waddles.push(waddle);
        }
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        let mut rows = conn
            .query(query, libsql::params![user_did, limit as i64, offset as i64])
            .await
            .map_err(|e| format!("Failed to query waddles: {}", e))?;

        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read waddle row: {}", e))?
        {
            let waddle = parse_waddle_row(&row)?;
            waddles.push(waddle);
        }
    }

    Ok(waddles)
}

/// Parse a waddle row from the list query
fn parse_waddle_row(row: &libsql::Row) -> Result<WaddleResponse, String> {
    let id: String = row
        .get(0)
        .map_err(|e| format!("Failed to get id: {}", e))?;
    let name: String = row
        .get(1)
        .map_err(|e| format!("Failed to get name: {}", e))?;
    let description: Option<String> = row.get(2).ok();
    let owner_did: String = row
        .get(3)
        .map_err(|e| format!("Failed to get owner_did: {}", e))?;
    let icon_url: Option<String> = row.get(4).ok();
    let is_public: i32 = row
        .get(5)
        .map_err(|e| format!("Failed to get is_public: {}", e))?;
    let created_at: String = row
        .get(6)
        .map_err(|e| format!("Failed to get created_at: {}", e))?;
    let updated_at: Option<String> = row.get(7).ok();
    let role: Option<String> = row.get(8).ok();

    Ok(WaddleResponse {
        id,
        name,
        description,
        owner_did,
        icon_url,
        is_public: is_public != 0,
        role,
        created_at,
        updated_at,
    })
}

// === Member Management Database Helper Functions ===

/// List members of a waddle with pagination
async fn list_waddle_members(
    db: &Database,
    waddle_id: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<MemberResponse>, String> {
    let query = r#"
        SELECT u.did, u.handle, wm.role, wm.joined_at
        FROM waddle_members wm
        JOIN users u ON wm.user_id = u.id
        WHERE wm.waddle_id = ?
        ORDER BY
            CASE wm.role
                WHEN 'owner' THEN 1
                WHEN 'admin' THEN 2
                WHEN 'moderator' THEN 3
                ELSE 4
            END,
            wm.joined_at ASC
        LIMIT ? OFFSET ?
    "#;

    let mut members = Vec::new();

    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        let mut rows = conn
            .query(query, libsql::params![waddle_id, limit as i64, offset as i64])
            .await
            .map_err(|e| format!("Failed to query members: {}", e))?;

        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read member row: {}", e))?
        {
            let member = parse_member_row(&row)?;
            members.push(member);
        }
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        let mut rows = conn
            .query(query, libsql::params![waddle_id, limit as i64, offset as i64])
            .await
            .map_err(|e| format!("Failed to query members: {}", e))?;

        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read member row: {}", e))?
        {
            let member = parse_member_row(&row)?;
            members.push(member);
        }
    }

    Ok(members)
}

/// Parse a member row from the database
fn parse_member_row(row: &libsql::Row) -> Result<MemberResponse, String> {
    let did: String = row
        .get(0)
        .map_err(|e| format!("Failed to get did: {}", e))?;
    let handle: String = row
        .get(1)
        .map_err(|e| format!("Failed to get handle: {}", e))?;
    let role: String = row
        .get(2)
        .map_err(|e| format!("Failed to get role: {}", e))?;
    let joined_at: String = row
        .get(3)
        .map_err(|e| format!("Failed to get joined_at: {}", e))?;

    Ok(MemberResponse {
        did,
        handle,
        role,
        joined_at,
    })
}

/// Get or create a user by DID (used when adding members who may not exist yet)
async fn get_or_create_user_by_did(db: &Database, did: &str) -> Result<i64, String> {
    // Try to get existing user
    let select_query = "SELECT id FROM users WHERE did = ?";

    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        let mut rows = conn
            .query(select_query, libsql::params![did])
            .await
            .map_err(|e| format!("Failed to query user: {}", e))?;

        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read user row: {}", e))?
        {
            return row
                .get(0)
                .map_err(|e| format!("Failed to get user id: {}", e));
        }

        // User doesn't exist, create with DID as handle placeholder
        let insert_query = "INSERT INTO users (did, handle) VALUES (?, ?)";
        conn.execute(insert_query, libsql::params![did, did])
            .await
            .map_err(|e| format!("Failed to create user: {}", e))?;

        // Get the new user ID
        let mut rows = conn
            .query(select_query, libsql::params![did])
            .await
            .map_err(|e| format!("Failed to query new user: {}", e))?;

        let row = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read new user row: {}", e))?
            .ok_or_else(|| "User not found after insert".to_string())?;

        row.get(0)
            .map_err(|e| format!("Failed to get new user id: {}", e))
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        let mut rows = conn
            .query(select_query, libsql::params![did])
            .await
            .map_err(|e| format!("Failed to query user: {}", e))?;

        if let Some(row) = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read user row: {}", e))?
        {
            return row
                .get(0)
                .map_err(|e| format!("Failed to get user id: {}", e));
        }

        // User doesn't exist, create with DID as handle placeholder
        let insert_query = "INSERT INTO users (did, handle) VALUES (?, ?)";
        conn.execute(insert_query, libsql::params![did, did])
            .await
            .map_err(|e| format!("Failed to create user: {}", e))?;

        // Get the new user ID
        let mut rows = conn
            .query(select_query, libsql::params![did])
            .await
            .map_err(|e| format!("Failed to query new user: {}", e))?;

        let row = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read new user row: {}", e))?
            .ok_or_else(|| "User not found after insert".to_string())?;

        row.get(0)
            .map_err(|e| format!("Failed to get new user id: {}", e))
    }
}

/// Get member's role in a waddle (returns None if not a member)
async fn get_member_role(
    db: &Database,
    waddle_id: &str,
    user_did: &str,
) -> Result<Option<String>, String> {
    let query = r#"
        SELECT wm.role
        FROM waddle_members wm
        JOIN users u ON wm.user_id = u.id
        WHERE wm.waddle_id = ? AND u.did = ?
    "#;

    let row = if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        let mut rows = conn
            .query(query, libsql::params![waddle_id, user_did])
            .await
            .map_err(|e| format!("Failed to query member role: {}", e))?;

        rows.next()
            .await
            .map_err(|e| format!("Failed to read member role row: {}", e))?
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        let mut rows = conn
            .query(query, libsql::params![waddle_id, user_did])
            .await
            .map_err(|e| format!("Failed to query member role: {}", e))?;

        rows.next()
            .await
            .map_err(|e| format!("Failed to read member role row: {}", e))?
    };

    match row {
        Some(row) => {
            let role: String = row
                .get(0)
                .map_err(|e| format!("Failed to get role: {}", e))?;
            Ok(Some(role))
        }
        None => Ok(None),
    }
}

/// Add a member to a waddle with a specific timestamp
async fn add_waddle_member_with_timestamp(
    db: &Database,
    waddle_id: &str,
    user_id: i64,
    role: &str,
    joined_at: &str,
) -> Result<(), String> {
    let query = r#"
        INSERT INTO waddle_members (waddle_id, user_id, role, joined_at)
        VALUES (?, ?, ?, ?)
    "#;

    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        conn.execute(query, libsql::params![waddle_id, user_id, role, joined_at])
            .await
            .map_err(|e| format!("Failed to add waddle member: {}", e))?;
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        conn.execute(query, libsql::params![waddle_id, user_id, role, joined_at])
            .await
            .map_err(|e| format!("Failed to add waddle member: {}", e))?;
    }

    Ok(())
}

/// Remove a member from a waddle
async fn remove_waddle_member(
    db: &Database,
    waddle_id: &str,
    user_did: &str,
) -> Result<(), String> {
    let query = r#"
        DELETE FROM waddle_members
        WHERE waddle_id = ? AND user_id = (SELECT id FROM users WHERE did = ?)
    "#;

    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        conn.execute(query, libsql::params![waddle_id, user_did])
            .await
            .map_err(|e| format!("Failed to remove waddle member: {}", e))?;
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        conn.execute(query, libsql::params![waddle_id, user_did])
            .await
            .map_err(|e| format!("Failed to remove waddle member: {}", e))?;
    }

    Ok(())
}

/// Get user handle by DID
async fn get_user_handle(db: &Database, did: &str) -> Result<String, String> {
    let query = "SELECT handle FROM users WHERE did = ?";

    if let Some(persistent) = db.persistent_connection() {
        let conn = persistent.lock().await;
        let mut rows = conn
            .query(query, libsql::params![did])
            .await
            .map_err(|e| format!("Failed to query user handle: {}", e))?;

        let row = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read user handle row: {}", e))?
            .ok_or_else(|| "User not found".to_string())?;

        row.get(0)
            .map_err(|e| format!("Failed to get handle: {}", e))
    } else {
        let conn = db
            .connect()
            .map_err(|e| format!("Failed to connect to database: {}", e))?;

        let mut rows = conn
            .query(query, libsql::params![did])
            .await
            .map_err(|e| format!("Failed to query user handle: {}", e))?;

        let row = rows
            .next()
            .await
            .map_err(|e| format!("Failed to read user handle row: {}", e))?
            .ok_or_else(|| "User not found".to_string())?;

        row.get(0)
            .map_err(|e| format!("Failed to get handle: {}", e))
    }
}

/// Create the default #general channel in a per-waddle database
async fn create_default_channel(waddle_db: &Database) -> Result<(), String> {
    let channel_id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let query = r#"
        INSERT INTO channels (id, name, description, channel_type, position, is_default, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
    "#;

    if let Some(persistent) = waddle_db.persistent_connection() {
        let conn = persistent.lock().await;
        conn.execute(
            query,
            libsql::params![channel_id, "general", "General discussion", "text", 0, 1, now.clone(), now],
        )
        .await
        .map_err(|e| format!("Failed to create default channel: {}", e))?;
    } else {
        let conn = waddle_db
            .connect()
            .map_err(|e| format!("Failed to connect to waddle database: {}", e))?;

        conn.execute(
            query,
            libsql::params![channel_id, "general", "General discussion", "text", 0, 1, now.clone(), now],
        )
        .await
        .map_err(|e| format!("Failed to create default channel: {}", e))?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Session;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use axum::body::Body;
    use axum::http::Request;
    use chrono::{Duration, Utc};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn create_test_waddle_state() -> Arc<WaddleState> {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let db_pool = DatabasePool::new(config, pool_config).await.unwrap();

        // Run migrations
        let runner = MigrationRunner::global();
        runner.run(db_pool.global()).await.unwrap();

        let app_state = Arc::new(AppState::new(db_pool));
        Arc::new(WaddleState::new(
            app_state,
            Some(b"test-encryption-key-32-bytes!!!"),
        ))
    }

    async fn create_test_session(state: &WaddleState) -> Session {
        let session = Session {
            id: format!("test-session-{}", Uuid::new_v4()),
            did: format!("did:plc:test{}", Uuid::new_v4().to_string().replace("-", "")[..16].to_string()),
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

    #[tokio::test]
    async fn test_create_waddle() {
        let waddle_state = create_test_waddle_state().await;
        let session = create_test_session(&waddle_state).await;
        let app = router(waddle_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/waddles?session_id={}", session.id))
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{"name": "Test Waddle", "description": "A test waddle"}"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["name"], "Test Waddle");
        assert_eq!(json["description"], "A test waddle");
        assert_eq!(json["owner_did"], session.did);
        assert_eq!(json["role"], "owner");
        assert!(json["is_public"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_create_waddle_missing_session() {
        let waddle_state = create_test_waddle_state().await;
        let app = router(waddle_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/waddles?session_id=nonexistent")
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "Test Waddle"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_create_waddle_empty_name() {
        let waddle_state = create_test_waddle_state().await;
        let session = create_test_session(&waddle_state).await;
        let app = router(waddle_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/waddles?session_id={}", session.id))
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
    async fn test_get_waddle() {
        let waddle_state = create_test_waddle_state().await;
        let session = create_test_session(&waddle_state).await;
        let app = router(waddle_state.clone());

        // Create a waddle first
        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/waddles?session_id={}", session.id))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "Test Waddle"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        let create_status = create_response.status();
        let body = create_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let create_json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        eprintln!("Create response status: {:?}", create_status);
        eprintln!("Create response body: {:?}", create_json);
        assert_eq!(create_status, StatusCode::CREATED, "Create waddle failed: {:?}", create_json);
        let waddle_id = create_json["id"].as_str().unwrap();

        // Get the waddle
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/v1/waddles/{}?session_id={}",
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

        assert_eq!(json["id"], waddle_id);
        assert_eq!(json["name"], "Test Waddle");
    }

    #[tokio::test]
    async fn test_get_waddle_not_found() {
        let waddle_state = create_test_waddle_state().await;
        let session = create_test_session(&waddle_state).await;
        let app = router(waddle_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/v1/waddles/nonexistent?session_id={}",
                        session.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_update_waddle() {
        let waddle_state = create_test_waddle_state().await;
        let session = create_test_session(&waddle_state).await;
        let app = router(waddle_state.clone());

        // Create a waddle first
        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/waddles?session_id={}", session.id))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "Test Waddle"}"#))
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
        let waddle_id = create_json["id"].as_str().unwrap();

        // Update the waddle
        let response = app
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!(
                        "/v1/waddles/{}?session_id={}",
                        waddle_id, session.id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "Updated Waddle", "description": "New description"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["name"], "Updated Waddle");
        assert_eq!(json["description"], "New description");
    }

    #[tokio::test]
    async fn test_delete_waddle() {
        let waddle_state = create_test_waddle_state().await;
        let session = create_test_session(&waddle_state).await;
        let app = router(waddle_state.clone());

        // Create a waddle first
        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/waddles?session_id={}", session.id))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "Test Waddle"}"#))
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
        let waddle_id = create_json["id"].as_str().unwrap();

        // Delete the waddle
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!(
                        "/v1/waddles/{}?session_id={}",
                        waddle_id, session.id
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
                        "/v1/waddles/{}?session_id={}",
                        waddle_id, session.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn test_list_waddles() {
        let waddle_state = create_test_waddle_state().await;
        let session = create_test_session(&waddle_state).await;
        let app = router(waddle_state.clone());

        // Create two waddles
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/waddles?session_id={}", session.id))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "Waddle 1"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/waddles?session_id={}", session.id))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "Waddle 2"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        // List waddles
        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/v1/waddles?session_id={}", session.id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["total"], 2);
        assert_eq!(json["waddles"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_delete_waddle_permission_denied() {
        let waddle_state = create_test_waddle_state().await;
        let owner_session = create_test_session(&waddle_state).await;
        let other_session = create_test_session(&waddle_state).await;
        let app = router(waddle_state.clone());

        // Create a waddle as owner
        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/waddles?session_id={}", owner_session.id))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "Owner's Waddle"}"#))
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
        let waddle_id = create_json["id"].as_str().unwrap();

        // Try to delete as a different user
        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri(format!(
                        "/v1/waddles/{}?session_id={}",
                        waddle_id, other_session.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
}
