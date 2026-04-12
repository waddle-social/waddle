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
//! - PATCH /v1/waddles/:id/members/:user_id - Update a member role
//! - DELETE /v1/waddles/:id/members/:user_id - Remove a member from the waddle

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
    /// When true, only one waddle is allowed (single-tenant mode).
    pub single_tenant: bool,
}

impl WaddleState {
    /// Create new waddle state
    pub fn new(
        app_state: Arc<AppState>,
        encryption_key: Option<&[u8]>,
        single_tenant: bool,
    ) -> Self {
        let db = Arc::new(app_state.db_pool.global().clone());
        let permission_service = PermissionService::new(Arc::clone(&db));
        let session_manager = SessionManager::new(Arc::clone(&db), encryption_key);
        Self {
            app_state,
            permission_service,
            session_manager,
            single_tenant,
        }
    }
}

/// Create the waddles router
pub fn router(waddle_state: Arc<WaddleState>) -> Router {
    Router::new()
        .route("/v1/waddles/public", get(list_public_waddles_handler))
        .route("/v1/waddles", post(create_waddle_handler))
        .route("/v1/waddles/:id/join", post(join_public_waddle_handler))
        .route("/v1/waddles/:id", patch(update_waddle_handler))
        .route("/v1/waddles/:id", delete(delete_waddle_handler))
        // Member management routes
        .route("/v1/waddles/:id/members", get(list_members_handler))
        .route("/v1/waddles/:id/members", post(add_member_handler))
        .route(
            "/v1/waddles/:id/members/:member_user_id",
            patch(update_member_role_handler).delete(remove_member_handler),
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
    /// Owner user ID
    pub owner_user_id: String,
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

/// Query parameters for session authentication
#[derive(Debug, Deserialize)]
pub struct SessionQuery {
    /// Session ID for authentication
    pub session_id: String,
}

/// Query parameters for public-space browsing.
#[derive(Debug, Deserialize)]
pub struct ListPublicWaddlesQuery {
    /// Session ID for authentication
    pub session_id: String,
    /// Optional search term over space name/id
    pub query: Option<String>,
    /// Maximum number of results (default: 50)
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// Offset for pagination (default: 0)
    #[serde(default)]
    pub offset: usize,
}

/// Response payload for public-space browsing.
#[derive(Debug, Serialize)]
pub struct ListPublicWaddlesResponse {
    /// Public spaces matching the query.
    pub waddles: Vec<WaddleResponse>,
    /// Number of returned rows.
    pub total: usize,
}

fn default_limit() -> usize {
    50
}

// === Member Management Request/Response Types ===

/// Request body for adding a member to a waddle
#[derive(Debug, Deserialize)]
pub struct AddMemberRequest {
    /// User ID of the user to add as a member
    pub user_id: String,
    /// Role for the new member (default: "member")
    #[serde(default = "default_member_role")]
    pub role: String,
}

fn default_member_role() -> String {
    "member".to_string()
}

/// Request body for updating a member role.
#[derive(Debug, Deserialize)]
pub struct UpdateMemberRoleRequest {
    /// Updated role for the member.
    pub role: String,
}

/// Response for a single waddle member
#[derive(Debug, Serialize)]
pub struct MemberResponse {
    /// User ID
    pub user_id: String,
    /// User's immutable username
    pub username: String,
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
    /// Member user ID (URL-encoded)
    pub member_user_id: String,
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

/// GET /v1/waddles/public
///
/// Browse discoverable public spaces, optionally filtered by query text.
#[instrument(skip(state))]
pub async fn list_public_waddles_handler(
    State(state): State<Arc<WaddleState>>,
    Query(params): Query<ListPublicWaddlesQuery>,
) -> impl IntoResponse {
    debug!("Listing public waddles");

    let session = match state
        .session_manager
        .validate_session(&params.session_id)
        .await
    {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return waddle_error_to_response(WaddleError::Auth(err)).into_response();
        }
    };

    let mut waddles = match list_public_waddles_from_db(
        state.app_state.db_pool.global(),
        params.query.as_deref(),
        params.limit,
        params.offset,
    )
    .await
    {
        Ok(rows) => rows,
        Err(err) => {
            error!("Failed to list public waddles: {}", err);
            return waddle_error_to_response(WaddleError::Database(err)).into_response();
        }
    };

    for waddle in &mut waddles {
        if let Ok(role) = get_user_role(
            state.app_state.db_pool.global(),
            &waddle.id,
            &session.user_id,
        )
        .await
        {
            waddle.role = role;
        }
    }

    let total = waddles.len();
    (
        StatusCode::OK,
        Json(ListPublicWaddlesResponse { waddles, total }),
    )
        .into_response()
}

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

    // Single-tenant guard: reject creation when a waddle already exists
    if state.single_tenant {
        match list_all_waddles_from_db(state.app_state.db_pool.global(), 1, 0).await {
            Ok(rows) if !rows.is_empty() => {
                warn!("Rejected waddle creation: single-tenant mode and a waddle already exists");
                return (
                    StatusCode::FORBIDDEN,
                    Json(serde_json::json!({
                        "error": "Single-tenant mode: only one waddle is allowed"
                    })),
                )
                    .into_response();
            }
            _ => {}
        }
    }

    // Validate session
    let session = match state
        .session_manager
        .validate_session(&params.session_id)
        .await
    {
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

    let user_id = session.user_id.clone();

    // Insert waddle into database
    let now = chrono::Utc::now().to_rfc3339();
    if let Err(err) = insert_waddle(
        state.app_state.db_pool.global(),
        &waddle_id,
        &request.name,
        request.description.as_deref(),
        &user_id,
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
    if let Err(err) = add_waddle_member(
        state.app_state.db_pool.global(),
        &waddle_id,
        &user_id,
        "owner",
    )
    .await
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
        Subject::user(&session.user_id),
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
            if let Err(err) =
                create_default_channel(&waddle_db, &state.permission_service, &waddle_id).await
            {
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
            owner_user_id: session.user_id,
            icon_url: request.icon_url,
            is_public: request.is_public,
            role: Some("owner".to_string()),
            created_at: now.clone(),
            updated_at: Some(now),
        }),
    )
        .into_response()
}

/// POST /v1/waddles/:id/join
///
/// Join a public waddle as a member using the authenticated session user.
#[instrument(skip(state))]
pub async fn join_public_waddle_handler(
    State(state): State<Arc<WaddleState>>,
    Path(waddle_id): Path<String>,
    Query(params): Query<SessionQuery>,
) -> impl IntoResponse {
    info!("Joining public waddle: {}", waddle_id);

    let session = match state
        .session_manager
        .validate_session(&params.session_id)
        .await
    {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return waddle_error_to_response(WaddleError::Auth(err)).into_response();
        }
    };

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

    if !waddle.is_public {
        return waddle_error_to_response(WaddleError::Permission(PermissionError::Denied(
            "Only public waddles can be joined directly".to_string(),
        )))
        .into_response();
    }

    let existing_role = match get_member_role(
        state.app_state.db_pool.global(),
        &waddle_id,
        &session.user_id,
    )
    .await
    {
        Ok(role) => role,
        Err(err) => {
            error!("Failed to check existing membership: {}", err);
            return waddle_error_to_response(WaddleError::Database(err)).into_response();
        }
    };

    if let Some(role) = existing_role {
        return (
            StatusCode::OK,
            Json(WaddleResponse {
                role: Some(role),
                ..waddle
            }),
        )
            .into_response();
    }

    let now = chrono::Utc::now().to_rfc3339();
    if let Err(err) = add_waddle_member_with_timestamp(
        state.app_state.db_pool.global(),
        &waddle_id,
        &session.user_id,
        "member",
        &now,
    )
    .await
    {
        error!("Failed to add public waddle member: {}", err);
        return waddle_error_to_response(WaddleError::Database(err)).into_response();
    }

    let tuple = Tuple::new(
        Object::new(ObjectType::Waddle, &waddle_id),
        Relation::new("member"),
        Subject::user(&session.user_id),
    );

    if let Err(err) = state.permission_service.write_tuple(tuple).await {
        error!("Failed to write member permission tuple: {}", err);
        let _ = remove_waddle_member(
            state.app_state.db_pool.global(),
            &waddle_id,
            &session.user_id,
        )
        .await;
        return waddle_error_to_response(WaddleError::Permission(err)).into_response();
    }

    (
        StatusCode::OK,
        Json(WaddleResponse {
            role: Some("member".to_string()),
            ..waddle
        }),
    )
        .into_response()
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
    let session = match state
        .session_manager
        .validate_session(&params.session_id)
        .await
    {
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
    let subject = Subject::user(&session.user_id);
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
    let role = get_user_role(
        state.app_state.db_pool.global(),
        &waddle_id,
        &session.user_id,
    )
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
    let session = match state
        .session_manager
        .validate_session(&params.session_id)
        .await
    {
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
    let subject = Subject::user(&session.user_id);
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
        Subject::user(&session.user_id),
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
    let session = match state
        .session_manager
        .validate_session(&params.session_id)
        .await
    {
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
    let subject = Subject::user(&session.user_id);
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

    (StatusCode::OK, Json(ListMembersResponse { members, total })).into_response()
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
    info!("Adding member {} to waddle {}", request.user_id, waddle_id);

    // Validate session
    let session = match state
        .session_manager
        .validate_session(&params.session_id)
        .await
    {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return waddle_error_to_response(WaddleError::Auth(err)).into_response();
        }
    };

    // Validate input
    if request.user_id.trim().is_empty() {
        return waddle_error_to_response(WaddleError::InvalidInput(
            "user_id cannot be empty".to_string(),
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
    let subject = Subject::user(&session.user_id);
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

    // Check if user being added exists and get username for response
    let username =
        match get_username_by_user_id(state.app_state.db_pool.global(), &request.user_id).await {
            Ok(Some(username)) => username,
            Ok(None) => {
                return waddle_error_to_response(WaddleError::NotFound(format!(
                    "User '{}' not found",
                    request.user_id
                )))
                .into_response();
            }
            Err(err) => {
                error!("Failed to lookup user: {}", err);
                return waddle_error_to_response(WaddleError::Database(err)).into_response();
            }
        };

    // Check if member already exists in waddle
    if let Ok(Some(_)) = get_member_role(
        state.app_state.db_pool.global(),
        &waddle_id,
        &request.user_id,
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
        &request.user_id,
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
        Subject::user(&request.user_id),
    );

    if let Err(err) = state.permission_service.write_tuple(member_tuple).await {
        error!("Failed to write member permission tuple: {}", err);
        // Clean up: remove the member from the database
        let _ = remove_waddle_member(
            state.app_state.db_pool.global(),
            &waddle_id,
            &request.user_id,
        )
        .await;
        return waddle_error_to_response(WaddleError::Permission(err)).into_response();
    }

    info!("Member {} added to waddle {}", request.user_id, waddle_id);

    (
        StatusCode::CREATED,
        Json(MemberResponse {
            user_id: request.user_id,
            username,
            role: request.role,
            joined_at: now,
        }),
    )
        .into_response()
}

/// PATCH /v1/waddles/:id/members/:member_user_id
///
/// Update a member role in a waddle. Requires manage_members permission.
/// The owner role cannot be assigned or changed via this endpoint.
#[instrument(skip(state))]
pub async fn update_member_role_handler(
    State(state): State<Arc<WaddleState>>,
    Path(path): Path<MemberPath>,
    Query(params): Query<SessionQuery>,
    Json(request): Json<UpdateMemberRoleRequest>,
) -> impl IntoResponse {
    let member_user_id = percent_encoding::percent_decode_str(&path.member_user_id)
        .decode_utf8()
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| path.member_user_id.clone());

    info!(
        "Updating member {} role to {} in waddle {}",
        member_user_id, request.role, path.id
    );

    let session = match state
        .session_manager
        .validate_session(&params.session_id)
        .await
    {
        Ok(session) => session,
        Err(err) => {
            warn!("Session validation failed: {}", err);
            return waddle_error_to_response(WaddleError::Auth(err)).into_response();
        }
    };

    let valid_roles = ["member", "moderator", "admin"];
    if !valid_roles.contains(&request.role.as_str()) {
        return waddle_error_to_response(WaddleError::InvalidInput(format!(
            "Invalid role '{}'. Valid roles are: member, moderator, admin",
            request.role
        )))
        .into_response();
    }

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

    if member_user_id == waddle.owner_user_id {
        return waddle_error_to_response(WaddleError::InvalidInput(
            "Cannot change the owner role via the member API".to_string(),
        ))
        .into_response();
    }

    let subject = Subject::user(&session.user_id);
    let object = Object::new(ObjectType::Waddle, &path.id);
    let can_manage = state
        .permission_service
        .check(&subject, "manage_members", &object)
        .await
        .map(|response| response.allowed)
        .unwrap_or(false);

    if !can_manage {
        return waddle_error_to_response(WaddleError::Permission(PermissionError::Denied(
            "You do not have permission to manage members in this waddle".to_string(),
        )))
        .into_response();
    }

    let current_role =
        match get_member_role(state.app_state.db_pool.global(), &path.id, &member_user_id).await {
            Ok(Some(role)) => role,
            Ok(None) => {
                return waddle_error_to_response(WaddleError::NotFound(format!(
                    "Member '{}' not found in waddle",
                    member_user_id
                )))
                .into_response();
            }
            Err(err) => {
                error!("Failed to get member role: {}", err);
                return waddle_error_to_response(WaddleError::Database(err)).into_response();
            }
        };

    if current_role == request.role {
        let member =
            match get_waddle_member(state.app_state.db_pool.global(), &path.id, &member_user_id)
                .await
            {
                Ok(Some(member)) => member,
                Ok(None) => {
                    return waddle_error_to_response(WaddleError::NotFound(format!(
                        "Member '{}' not found in waddle",
                        member_user_id
                    )))
                    .into_response();
                }
                Err(err) => {
                    error!("Failed to get member: {}", err);
                    return waddle_error_to_response(WaddleError::Database(err)).into_response();
                }
            };

        return (StatusCode::OK, Json(member)).into_response();
    }

    let old_tuple = Tuple::new(
        Object::new(ObjectType::Waddle, &path.id),
        Relation::new(&current_role),
        Subject::user(&member_user_id),
    );

    if let Err(err) = state.permission_service.delete_tuple(&old_tuple).await {
        error!("Failed to delete old member permission tuple: {}", err);
        return waddle_error_to_response(WaddleError::Permission(err)).into_response();
    }

    let new_tuple = Tuple::new(
        Object::new(ObjectType::Waddle, &path.id),
        Relation::new(&request.role),
        Subject::user(&member_user_id),
    );

    if let Err(err) = state
        .permission_service
        .write_tuple(new_tuple.clone())
        .await
    {
        error!("Failed to write updated member permission tuple: {}", err);
        let _ = state.permission_service.write_tuple(old_tuple).await;
        return waddle_error_to_response(WaddleError::Permission(err)).into_response();
    }

    if let Err(err) = update_waddle_member_role(
        state.app_state.db_pool.global(),
        &path.id,
        &member_user_id,
        &request.role,
    )
    .await
    {
        error!("Failed to update member role: {}", err);
        let _ = state.permission_service.delete_tuple(&new_tuple).await;
        let _ = state
            .permission_service
            .write_tuple(Tuple::new(
                Object::new(ObjectType::Waddle, &path.id),
                Relation::new(&current_role),
                Subject::user(&member_user_id),
            ))
            .await;
        return waddle_error_to_response(WaddleError::Database(err)).into_response();
    }

    let member = match get_waddle_member(
        state.app_state.db_pool.global(),
        &path.id,
        &member_user_id,
    )
    .await
    {
        Ok(Some(member)) => member,
        Ok(None) => {
            return waddle_error_to_response(WaddleError::NotFound(format!(
                "Member '{}' not found in waddle",
                member_user_id
            )))
            .into_response();
        }
        Err(err) => {
            error!("Failed to get updated member: {}", err);
            return waddle_error_to_response(WaddleError::Database(err)).into_response();
        }
    };

    (StatusCode::OK, Json(member)).into_response()
}

/// DELETE /v1/waddles/:id/members/:member_user_id
///
/// Remove a member from a waddle. Requires manage_members permission (owner/admin/moderator).
/// The owner cannot be removed from the waddle.
#[instrument(skip(state))]
pub async fn remove_member_handler(
    State(state): State<Arc<WaddleState>>,
    Path(path): Path<MemberPath>,
    Query(params): Query<SessionQuery>,
) -> impl IntoResponse {
    // URL-decode the member user_id
    let member_user_id = percent_encoding::percent_decode_str(&path.member_user_id)
        .decode_utf8()
        .map(|s| s.into_owned())
        .unwrap_or_else(|_| path.member_user_id.clone());

    info!("Removing member {} from waddle {}", member_user_id, path.id);

    // Validate session
    let session = match state
        .session_manager
        .validate_session(&params.session_id)
        .await
    {
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
    if member_user_id == waddle.owner_user_id {
        return waddle_error_to_response(WaddleError::InvalidInput(
            "Cannot remove the owner from the waddle".to_string(),
        ))
        .into_response();
    }

    // Check if user has permission to manage members
    let subject = Subject::user(&session.user_id);
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
    let member_role =
        match get_member_role(state.app_state.db_pool.global(), &path.id, &member_user_id).await {
            Ok(Some(role)) => role,
            Ok(None) => {
                return waddle_error_to_response(WaddleError::NotFound(format!(
                    "Member '{}' not found in waddle",
                    member_user_id
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
        Subject::user(&member_user_id),
    );

    if let Err(err) = state.permission_service.delete_tuple(&member_tuple).await {
        warn!("Failed to delete member permission tuple: {}", err);
        // Continue - we still want to remove from database
    }

    // Remove member from waddle_members table
    if let Err(err) =
        remove_waddle_member(state.app_state.db_pool.global(), &path.id, &member_user_id).await
    {
        error!("Failed to remove member: {}", err);
        return waddle_error_to_response(WaddleError::Database(err)).into_response();
    }

    info!("Member {} removed from waddle {}", member_user_id, path.id);

    StatusCode::NO_CONTENT.into_response()
}

// === Database Helper Functions ===

/// Insert a new waddle into the database
#[allow(clippy::too_many_arguments)]
async fn insert_waddle(
    db: &Database,
    id: &str,
    name: &str,
    description: Option<&str>,
    owner_id: &str,
    icon_url: Option<&str>,
    is_public: bool,
    now: &str,
) -> Result<(), String> {
    let query = r#"
        INSERT INTO waddles (id, name, description, owner_id, icon_url, is_public, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
    "#;

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
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

    Ok(())
}

/// Add a member to a waddle
async fn add_waddle_member(
    db: &Database,
    waddle_id: &str,
    user_id: &str,
    role: &str,
) -> Result<(), String> {
    let query = r#"
        INSERT INTO waddle_members (waddle_id, user_id, role)
        VALUES (?, ?, ?)
    "#;

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    conn.execute(query, libsql::params![waddle_id, user_id, role])
        .await
        .map_err(|e| format!("Failed to add waddle member: {}", e))?;

    Ok(())
}

/// Get a waddle from the database
async fn get_waddle_from_db(
    db: &Database,
    waddle_id: &str,
) -> Result<Option<WaddleResponse>, String> {
    let query = r#"
        SELECT w.id, w.name, w.description, u.id as owner_user_id, w.icon_url, w.is_public, w.created_at, w.updated_at
        FROM waddles w
        JOIN users u ON w.owner_id = u.id
        WHERE w.id = ?
    "#;

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    let mut rows = conn
        .query(query, libsql::params![waddle_id])
        .await
        .map_err(|e| format!("Failed to query waddle: {}", e))?;
    let row = rows
        .next()
        .await
        .map_err(|e| format!("Failed to read waddle row: {}", e))?;

    match row {
        Some(row) => {
            let id: String = row.get(0).map_err(|e| format!("Failed to get id: {}", e))?;
            let name: String = row
                .get(1)
                .map_err(|e| format!("Failed to get name: {}", e))?;
            let description: Option<String> = row.get(2).ok();
            let owner_user_id: String = row
                .get(3)
                .map_err(|e| format!("Failed to get owner_user_id: {}", e))?;
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
                owner_user_id,
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
    user_id: &str,
) -> Result<Option<String>, String> {
    let query = r#"
        SELECT wm.role
        FROM waddle_members wm
        WHERE wm.waddle_id = ? AND wm.user_id = ?
    "#;

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    let mut rows = conn
        .query(query, libsql::params![waddle_id, user_id])
        .await
        .map_err(|e| format!("Failed to query role: {}", e))?;
    let row = rows
        .next()
        .await
        .map_err(|e| format!("Failed to read role row: {}", e))?;

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

    let query = format!("UPDATE waddles SET {} WHERE id = ?", updates.join(", "));

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    conn.execute(&query, params)
        .await
        .map_err(|e| format!("Failed to update waddle: {}", e))?;

    Ok(())
}

/// Delete a waddle from the database
async fn delete_waddle_from_db(db: &Database, waddle_id: &str) -> Result<(), String> {
    // First delete from waddle_members (foreign key constraint)
    let delete_members_query = "DELETE FROM waddle_members WHERE waddle_id = ?";
    let delete_waddle_query = "DELETE FROM waddles WHERE id = ?";

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    conn.execute(delete_members_query, libsql::params![waddle_id])
        .await
        .map_err(|e| format!("Failed to delete waddle members: {}", e))?;
    conn.execute(delete_waddle_query, libsql::params![waddle_id])
        .await
        .map_err(|e| format!("Failed to delete waddle: {}", e))?;

    Ok(())
}

/// List waddles the user is a member of
pub(crate) async fn list_user_waddles(
    db: &Database,
    user_id: &str,
    limit: usize,
    offset: usize,
) -> Result<Vec<WaddleResponse>, String> {
    let query = r#"
        SELECT w.id, w.name, w.description, u.id as owner_user_id, w.icon_url, w.is_public, w.created_at, w.updated_at, wm.role
        FROM waddles w
        JOIN users u ON w.owner_id = u.id
        JOIN waddle_members wm ON w.id = wm.waddle_id
        WHERE wm.user_id = ?
        ORDER BY w.created_at DESC
        LIMIT ? OFFSET ?
    "#;

    let mut waddles = Vec::new();

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    let mut rows = conn
        .query(query, libsql::params![user_id, limit as i64, offset as i64])
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

    Ok(waddles)
}

/// Parse the core waddle fields from a row (columns 0-6, no role/updated_at).
///
/// Returns a `WaddleResponse` with `role` and `updated_at` set to `None`.
/// Callers that need those fields should override them after calling this.
fn parse_waddle_row_base(row: &libsql::Row) -> Result<WaddleResponse, String> {
    let id: String = row.get(0).map_err(|e| format!("Failed to get id: {}", e))?;
    let name: String = row
        .get(1)
        .map_err(|e| format!("Failed to get name: {}", e))?;
    let description: Option<String> = row.get(2).ok();
    let owner_user_id: String = row
        .get(3)
        .map_err(|e| format!("Failed to get owner_user_id: {}", e))?;
    let icon_url: Option<String> = row.get(4).ok();
    let is_public: i32 = row
        .get(5)
        .map_err(|e| format!("Failed to get is_public: {}", e))?;
    let created_at: String = row
        .get(6)
        .map_err(|e| format!("Failed to get created_at: {}", e))?;
    Ok(WaddleResponse {
        id,
        name,
        description,
        owner_user_id,
        icon_url,
        is_public: is_public != 0,
        role: None,
        created_at,
        updated_at: None,
    })
}

/// Parse a waddle row from the list query (includes updated_at and role columns).
fn parse_waddle_row(row: &libsql::Row) -> Result<WaddleResponse, String> {
    let mut waddle = parse_waddle_row_base(row)?;
    waddle.updated_at = row.get(7).ok();
    waddle.role = row.get(8).ok();
    Ok(waddle)
}

/// Get a single waddle by ID.
///
/// Used by the XEP-0503 spaces service to look up space metadata.
pub(crate) async fn get_waddle_by_id(
    db: &Database,
    waddle_id: &str,
) -> Result<Option<WaddleResponse>, String> {
    let query = r#"
        SELECT w.id, w.name, w.description, u.id as owner_user_id, w.icon_url, w.is_public, w.created_at
        FROM waddles w
        JOIN users u ON w.owner_id = u.id
        WHERE w.id = ?
    "#;

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    let mut rows = conn
        .query(query, libsql::params![waddle_id])
        .await
        .map_err(|e| format!("Failed to query waddle: {}", e))?;

    if let Some(row) = rows
        .next()
        .await
        .map_err(|e| format!("Failed to read waddle row: {}", e))?
    {
        Ok(Some(parse_waddle_row_base(&row)?))
    } else {
        Ok(None)
    }
}

/// List all waddles with pagination.
///
/// Used by the XEP-0503 spaces service for single-tenant public discovery.
pub(crate) async fn list_all_waddles_from_db(
    db: &Database,
    limit: usize,
    offset: usize,
) -> Result<Vec<WaddleResponse>, String> {
    let query = r#"
        SELECT w.id, w.name, w.description, u.id as owner_user_id, w.icon_url, w.is_public, w.created_at
        FROM waddles w
        JOIN users u ON w.owner_id = u.id
        ORDER BY w.created_at DESC
        LIMIT ? OFFSET ?
    "#;

    let mut waddles = Vec::new();

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    let mut rows = conn
        .query(query, libsql::params![limit as i64, offset as i64])
        .await
        .map_err(|e| format!("Failed to query waddles: {}", e))?;

    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| format!("Failed to read waddle row: {}", e))?
    {
        let waddle = parse_waddle_row_base(&row)?;
        waddles.push(waddle);
    }

    Ok(waddles)
}

/// List only public waddles with pagination.
///
/// Used by the XEP-0503 spaces service so users can discover public spaces
/// they are not yet a member of.
pub(crate) async fn list_public_waddles_from_db(
    db: &Database,
    query: Option<&str>,
    limit: usize,
    offset: usize,
) -> Result<Vec<WaddleResponse>, String> {
    let normalized_query = query
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("%{}%", value.to_lowercase()));

    let (sql, params): (&str, Vec<libsql::Value>) = if let Some(pattern) = normalized_query {
        (
            r#"
                SELECT w.id, w.name, w.description, u.id as owner_user_id, w.icon_url, w.is_public, w.created_at
                FROM waddles w
                JOIN users u ON w.owner_id = u.id
                WHERE w.is_public = 1
                  AND (LOWER(w.name) LIKE ? OR LOWER(w.id) LIKE ?)
                ORDER BY w.created_at DESC
                LIMIT ? OFFSET ?
            "#,
            vec![
                pattern.clone().into(),
                pattern.into(),
                (limit as i64).into(),
                (offset as i64).into(),
            ],
        )
    } else {
        (
            r#"
                SELECT w.id, w.name, w.description, u.id as owner_user_id, w.icon_url, w.is_public, w.created_at
                FROM waddles w
                JOIN users u ON w.owner_id = u.id
                WHERE w.is_public = 1
                ORDER BY w.created_at DESC
                LIMIT ? OFFSET ?
            "#,
            vec![(limit as i64).into(), (offset as i64).into()],
        )
    };

    let mut waddles = Vec::new();

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    let mut rows = conn
        .query(sql, params)
        .await
        .map_err(|e| format!("Failed to query public waddles: {}", e))?;

    while let Some(row) = rows
        .next()
        .await
        .map_err(|e| format!("Failed to read public waddle row: {}", e))?
    {
        let waddle = parse_waddle_row_base(&row)?;
        waddles.push(waddle);
    }

    Ok(waddles)
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
        SELECT u.id, u.username, wm.role, wm.joined_at
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

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    let mut rows = conn
        .query(
            query,
            libsql::params![waddle_id, limit as i64, offset as i64],
        )
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

    Ok(members)
}

/// Parse a member row from the database
fn parse_member_row(row: &libsql::Row) -> Result<MemberResponse, String> {
    let user_id: String = row
        .get(0)
        .map_err(|e| format!("Failed to get user_id: {}", e))?;
    let username: String = row
        .get(1)
        .map_err(|e| format!("Failed to get username: {}", e))?;
    let role: String = row
        .get(2)
        .map_err(|e| format!("Failed to get role: {}", e))?;
    let joined_at: String = row
        .get(3)
        .map_err(|e| format!("Failed to get joined_at: {}", e))?;

    Ok(MemberResponse {
        user_id,
        username,
        role,
        joined_at,
    })
}

/// Get a single member in a waddle.
async fn get_waddle_member(
    db: &Database,
    waddle_id: &str,
    user_id: &str,
) -> Result<Option<MemberResponse>, String> {
    let query = r#"
        SELECT u.id, u.username, wm.role, wm.joined_at
        FROM waddle_members wm
        JOIN users u ON wm.user_id = u.id
        WHERE wm.waddle_id = ? AND wm.user_id = ?
        LIMIT 1
    "#;

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    let mut rows = conn
        .query(query, libsql::params![waddle_id, user_id])
        .await
        .map_err(|e| format!("Failed to query member: {}", e))?;
    let row = rows
        .next()
        .await
        .map_err(|e| format!("Failed to read member row: {}", e))?;

    row.map(|row| parse_member_row(&row)).transpose()
}

/// Get member's role in a waddle (returns None if not a member)
async fn get_member_role(
    db: &Database,
    waddle_id: &str,
    user_id: &str,
) -> Result<Option<String>, String> {
    let query = r#"
        SELECT wm.role
        FROM waddle_members wm
        WHERE wm.waddle_id = ? AND wm.user_id = ?
    "#;

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    let mut rows = conn
        .query(query, libsql::params![waddle_id, user_id])
        .await
        .map_err(|e| format!("Failed to query member role: {}", e))?;
    let row = rows
        .next()
        .await
        .map_err(|e| format!("Failed to read member role row: {}", e))?;

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

/// Update a member role in a waddle.
async fn update_waddle_member_role(
    db: &Database,
    waddle_id: &str,
    user_id: &str,
    role: &str,
) -> Result<(), String> {
    let query = r#"
        UPDATE waddle_members
        SET role = ?
        WHERE waddle_id = ? AND user_id = ?
    "#;

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    conn.execute(query, libsql::params![role, waddle_id, user_id])
        .await
        .map_err(|e| format!("Failed to update waddle member role: {}", e))?;

    Ok(())
}

/// Add a member to a waddle with a specific timestamp
async fn add_waddle_member_with_timestamp(
    db: &Database,
    waddle_id: &str,
    user_id: &str,
    role: &str,
    joined_at: &str,
) -> Result<(), String> {
    let query = r#"
        INSERT INTO waddle_members (waddle_id, user_id, role, joined_at)
        VALUES (?, ?, ?, ?)
    "#;

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    conn.execute(query, libsql::params![waddle_id, user_id, role, joined_at])
        .await
        .map_err(|e| format!("Failed to add waddle member: {}", e))?;

    Ok(())
}

/// Remove a member from a waddle
async fn remove_waddle_member(db: &Database, waddle_id: &str, user_id: &str) -> Result<(), String> {
    let query = r#"
        DELETE FROM waddle_members WHERE waddle_id = ? AND user_id = ?
    "#;

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    conn.execute(query, libsql::params![waddle_id, user_id])
        .await
        .map_err(|e| format!("Failed to remove waddle member: {}", e))?;

    Ok(())
}

/// Get username by user ID.
async fn get_username_by_user_id(db: &Database, user_id: &str) -> Result<Option<String>, String> {
    let query = "SELECT username FROM users WHERE id = ?";

    let conn = db.guard().await.map_err(|e| format!("Failed to connect to database: {}", e))?;
    let mut rows = conn
        .query(query, libsql::params![user_id])
        .await
        .map_err(|e| format!("Failed to query username: {}", e))?;

    match rows
        .next()
        .await
        .map_err(|e| format!("Failed to read username row: {}", e))?
    {
        Some(row) => row
            .get(0)
            .map(Some)
            .map_err(|e| format!("Failed to get username: {}", e)),
        None => Ok(None),
    }
}

/// Create the default #general channel in a per-waddle database
async fn create_default_channel(
    waddle_db: &Database,
    permission_service: &PermissionService,
    waddle_id: &str,
) -> Result<String, String> {
    let channel_id = Uuid::new_v4().to_string();
    let now = chrono::Utc::now().to_rfc3339();

    let query = r#"
        INSERT INTO channels (id, name, description, channel_type, position, is_default, created_at, updated_at)
        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
    "#;

    let conn = waddle_db.guard().await.map_err(|e| format!("Failed to connect to waddle database: {}", e))?;
    conn.execute(
        query,
        libsql::params![
            channel_id.as_str(),
            "general",
            "General discussion",
            "text",
            0,
            1,
            now.clone(),
            now
        ],
    )
    .await
    .map_err(|e| format!("Failed to create default channel: {}", e))?;

    let parent_tuple = Tuple::new(
        Object::new(ObjectType::Channel, &channel_id),
        Relation::new("parent"),
        Subject {
            subject_type: crate::permissions::SubjectType::Waddle,
            id: waddle_id.to_string(),
            relation: None,
        },
    );

    if let Err(err) = permission_service.write_tuple(parent_tuple).await {
        let delete_query = "DELETE FROM channels WHERE id = ?";

        if let Ok(conn) = waddle_db.guard().await {
            let _ = conn.execute(delete_query, [channel_id.as_str()]).await;
        }

        return Err(format!(
            "Failed to create default channel permission tuple: {}",
            err
        ));
    }

    Ok(channel_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::auth::Session;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn create_test_waddle_state() -> Arc<WaddleState> {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let db_pool = DatabasePool::new(config, pool_config).await.unwrap();

        // Run migrations
        let runner = MigrationRunner::global();
        runner.run(db_pool.global()).await.unwrap();

        let app_state = Arc::new(AppState::new(Arc::new(db_pool)));
        Arc::new(WaddleState::new(
            app_state,
            Some(b"test-encryption-key-32-bytes!!!"),
            false,
        ))
    }

    async fn create_test_session(state: &WaddleState) -> Session {
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

    async fn get_default_channel_id(state: &WaddleState, waddle_id: &str) -> String {
        let waddle_db = state
            .app_state
            .db_pool
            .get_waddle_db(waddle_id)
            .await
            .unwrap();

        let conn = waddle_db.guard().await.unwrap();
        let mut rows = conn
            .query("SELECT id FROM channels WHERE is_default = 1 LIMIT 1", ())
            .await
            .unwrap();
        let row = rows.next().await.unwrap().unwrap();

        row.get(0).unwrap()
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
        assert_eq!(json["owner_user_id"], session.user_id);
        assert_eq!(json["role"], "owner");
        assert!(json["is_public"].as_bool().unwrap());
    }

    #[tokio::test]
    async fn test_create_waddle_owner_can_access_default_channel() {
        let waddle_state = create_test_waddle_state().await;
        let session = create_test_session(&waddle_state).await;
        let app = router(waddle_state.clone());

        let response = app
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

        assert_eq!(response.status(), StatusCode::CREATED);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let waddle_id = json["id"].as_str().unwrap().to_string();
        let default_channel_id = get_default_channel_id(&waddle_state, &waddle_id).await;

        let subject = Subject::user(&session.user_id);
        let channel = Object::new(ObjectType::Channel, &default_channel_id);

        let view = waddle_state
            .permission_service
            .check(&subject, "view", &channel)
            .await
            .unwrap();
        assert!(view.allowed);

        let send = waddle_state
            .permission_service
            .check(&subject, "send_message", &channel)
            .await
            .unwrap();
        assert!(send.allowed);
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
                    .body(Body::from(
                        r#"{"name": "Updated Waddle", "description": "New description"}"#,
                    ))
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

    #[tokio::test]
    async fn test_update_member_role() {
        let waddle_state = create_test_waddle_state().await;
        let owner_session = create_test_session(&waddle_state).await;
        let member_session = create_test_session(&waddle_state).await;
        let app = router(waddle_state.clone());

        let create_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/waddles?session_id={}", owner_session.id))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name": "Role Test"}"#))
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

        let add_member_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/waddles/{}/members?session_id={}",
                        waddle_id, owner_session.id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(format!(
                        r#"{{"user_id":"{}","role":"member"}}"#,
                        member_session.user_id
                    )))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(add_member_response.status(), StatusCode::CREATED);

        let update_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PATCH")
                    .uri(format!(
                        "/v1/waddles/{}/members/{}?session_id={}",
                        waddle_id, member_session.user_id, owner_session.id
                    ))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"role":"admin"}"#))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(update_response.status(), StatusCode::OK);
        let body = update_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["role"], "admin");

        let list_response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/v1/waddles/{}/members?session_id={}",
                        waddle_id, owner_session.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(list_response.status(), StatusCode::OK);
        let body = list_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        let members = json["members"].as_array().unwrap();
        let updated = members
            .iter()
            .find(|member| member["user_id"] == member_session.user_id)
            .unwrap();
        assert_eq!(updated["role"], "admin");
    }

    #[tokio::test]
    async fn test_list_public_waddles_only_returns_public_spaces() {
        let waddle_state = create_test_waddle_state().await;
        let owner_session = create_test_session(&waddle_state).await;
        let viewer_session = create_test_session(&waddle_state).await;
        let app = router(waddle_state.clone());

        let create_public = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/waddles?session_id={}", owner_session.id))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name":"Public Space","is_public":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_public.status(), StatusCode::CREATED);
        let public_body = create_public
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let public_json: serde_json::Value = serde_json::from_slice(&public_body).unwrap();
        let public_id = public_json["id"].as_str().unwrap().to_string();

        let create_private = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/waddles?session_id={}", owner_session.id))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name":"Private Space","is_public":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_private.status(), StatusCode::CREATED);
        let private_body = create_private
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let private_json: serde_json::Value = serde_json::from_slice(&private_body).unwrap();
        let private_id = private_json["id"].as_str().unwrap().to_string();

        let list_response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!(
                        "/v1/waddles/public?session_id={}",
                        viewer_session.id
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
        let rows = list_json["waddles"].as_array().unwrap();

        assert!(
            rows.iter().any(|row| row["id"] == public_id),
            "public space should be browseable"
        );
        assert!(
            rows.iter().all(|row| row["id"] != private_id),
            "private space must not appear in public browse"
        );
    }

    #[tokio::test]
    async fn test_join_public_waddle_allows_public_and_rejects_private() {
        let waddle_state = create_test_waddle_state().await;
        let owner_session = create_test_session(&waddle_state).await;
        let joiner_session = create_test_session(&waddle_state).await;
        let app = router(waddle_state.clone());

        let create_public = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/waddles?session_id={}", owner_session.id))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name":"Joinable","is_public":true}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_public.status(), StatusCode::CREATED);
        let public_body = create_public
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let public_json: serde_json::Value = serde_json::from_slice(&public_body).unwrap();
        let public_id = public_json["id"].as_str().unwrap().to_string();

        let create_private = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!("/v1/waddles?session_id={}", owner_session.id))
                    .header("Content-Type", "application/json")
                    .body(Body::from(r#"{"name":"Closed","is_public":false}"#))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(create_private.status(), StatusCode::CREATED);
        let private_body = create_private
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        let private_json: serde_json::Value = serde_json::from_slice(&private_body).unwrap();
        let private_id = private_json["id"].as_str().unwrap().to_string();

        let join_public = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/waddles/{}/join?session_id={}",
                        public_id, joiner_session.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(join_public.status(), StatusCode::OK);
        let join_public_body = join_public.into_body().collect().await.unwrap().to_bytes();
        let join_public_json: serde_json::Value =
            serde_json::from_slice(&join_public_body).unwrap();
        assert_eq!(join_public_json["id"], public_id);
        assert_eq!(join_public_json["role"], "member");

        let join_private = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri(format!(
                        "/v1/waddles/{}/join?session_id={}",
                        private_id, joiner_session.id
                    ))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(join_private.status(), StatusCode::FORBIDDEN);
    }
}
