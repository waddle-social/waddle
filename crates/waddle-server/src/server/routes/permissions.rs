//! Permission API Routes
//!
//! Provides HTTP endpoints for the Zanzibar-inspired permission system:
//! - POST /v1/permissions/check - Check a single permission
//! - GET /v1/permissions/list - List permissions/relations for a subject on an object
//! - POST /v1/permissions/tuples - Write a permission tuple
//! - DELETE /v1/permissions/tuples - Delete a permission tuple

use crate::permissions::{
    CheckResponse as PermCheckResponse, Object, PermissionError, PermissionService, Relation,
    Subject, Tuple,
};
use crate::server::AppState;
use axum::{
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{delete, get, post},
    Json, Router,
};
use serde::{Deserialize, Serialize};
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

/// Extended application state for permission routes
pub struct PermissionState {
    /// Core app state
    #[allow(dead_code)]
    pub app_state: Arc<AppState>,
    /// Permission service
    pub permission_service: PermissionService,
}

impl PermissionState {
    /// Create new permission state
    pub fn new(app_state: Arc<AppState>) -> Self {
        let db = Arc::new(app_state.db_pool.global().clone());
        let permission_service = PermissionService::new(db);
        Self {
            app_state,
            permission_service,
        }
    }
}

/// Create the permissions router
pub fn router(permission_state: Arc<PermissionState>) -> Router {
    Router::new()
        .route("/v1/permissions/check", post(check_handler))
        .route("/v1/permissions/list", get(list_handler))
        .route("/v1/permissions/tuples", post(write_tuple_handler))
        .route("/v1/permissions/tuples", delete(delete_tuple_handler))
        .with_state(permission_state)
}

// === Request/Response Types ===

/// Request body for permission check endpoint
#[derive(Debug, Deserialize)]
pub struct CheckRequest {
    /// Subject in format "type:id" (e.g., "user:did:plc:alice")
    pub subject: String,
    /// Permission to check (e.g., "view", "delete", "send_message")
    pub permission: String,
    /// Object in format "type:id" (e.g., "waddle:penguin-club")
    pub object: String,
}

/// Response for permission check endpoint
#[derive(Debug, Serialize)]
pub struct CheckResponse {
    /// Whether the permission is granted
    pub allowed: bool,
    /// Reason for the decision (if allowed)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl From<PermCheckResponse> for CheckResponse {
    fn from(resp: PermCheckResponse) -> Self {
        Self {
            allowed: resp.allowed,
            reason: resp.reason,
        }
    }
}

/// Query parameters for list endpoint
#[derive(Debug, Deserialize)]
pub struct ListQuery {
    /// Subject in format "type:id" (e.g., "user:did:plc:alice")
    pub subject: String,
    /// Object in format "type:id" (e.g., "waddle:penguin-club")
    pub object: String,
}

/// Response for list endpoint
#[derive(Debug, Serialize)]
pub struct ListResponse {
    /// Direct relations the subject has on the object
    pub relations: Vec<String>,
}

/// Request body for writing a tuple
#[derive(Debug, Deserialize)]
pub struct WriteTupleRequest {
    /// Object in format "type:id" (e.g., "waddle:penguin-club")
    pub object: String,
    /// Relation name (e.g., "owner", "member")
    pub relation: String,
    /// Subject in format "type:id" or "type:id#relation" for usersets
    pub subject: String,
}

/// Response for write tuple endpoint
#[derive(Debug, Serialize)]
pub struct WriteTupleResponse {
    /// Generated tuple ID
    pub id: String,
    /// The full tuple string representation
    pub tuple: String,
}

/// Request body for deleting a tuple
#[derive(Debug, Deserialize)]
pub struct DeleteTupleRequest {
    /// Object in format "type:id" (e.g., "waddle:penguin-club")
    pub object: String,
    /// Relation name (e.g., "owner", "member")
    pub relation: String,
    /// Subject in format "type:id" or "type:id#relation" for usersets
    pub subject: String,
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

/// Convert PermissionError to HTTP response
fn permission_error_to_response(err: PermissionError) -> (StatusCode, Json<ErrorResponse>) {
    let (status, error_code) = match &err {
        PermissionError::Denied(_) => (StatusCode::FORBIDDEN, "permission_denied"),
        PermissionError::InvalidTuple(_) => (StatusCode::BAD_REQUEST, "invalid_tuple"),
        PermissionError::InvalidObject(_) => (StatusCode::BAD_REQUEST, "invalid_object"),
        PermissionError::InvalidSubject(_) => (StatusCode::BAD_REQUEST, "invalid_subject"),
        PermissionError::InvalidRelation(_) => (StatusCode::BAD_REQUEST, "invalid_relation"),
        PermissionError::TupleNotFound => (StatusCode::NOT_FOUND, "tuple_not_found"),
        PermissionError::TupleAlreadyExists => (StatusCode::CONFLICT, "tuple_exists"),
        PermissionError::DatabaseError(_) => (StatusCode::INTERNAL_SERVER_ERROR, "database_error"),
        PermissionError::SchemaError(_) => (StatusCode::BAD_REQUEST, "schema_error"),
        PermissionError::MaxDepthExceeded(_) => {
            (StatusCode::INTERNAL_SERVER_ERROR, "max_depth_exceeded")
        }
    };

    (
        status,
        Json(ErrorResponse::new(error_code, &err.to_string())),
    )
}

// === Handlers ===

/// POST /v1/permissions/check
///
/// Check if a subject has a permission on an object.
#[instrument(skip(state))]
pub async fn check_handler(
    State(state): State<Arc<PermissionState>>,
    Json(request): Json<CheckRequest>,
) -> impl IntoResponse {
    debug!(
        "Permission check: {} {} on {}",
        request.subject, request.permission, request.object
    );

    // Parse subject
    let subject = match Subject::parse(&request.subject) {
        Ok(s) => s,
        Err(e) => {
            warn!("Invalid subject format: {}", e);
            return permission_error_to_response(e).into_response();
        }
    };

    // Parse object
    let object = match Object::parse(&request.object) {
        Ok(o) => o,
        Err(e) => {
            warn!("Invalid object format: {}", e);
            return permission_error_to_response(e).into_response();
        }
    };

    // Perform permission check
    match state
        .permission_service
        .check(&subject, &request.permission, &object)
        .await
    {
        Ok(result) => {
            debug!(
                "Permission check result: {} - allowed={}",
                request.permission, result.allowed
            );
            (StatusCode::OK, Json(CheckResponse::from(result))).into_response()
        }
        Err(e) => {
            error!("Permission check failed: {}", e);
            permission_error_to_response(e).into_response()
        }
    }
}

/// GET /v1/permissions/list
///
/// List all direct relations a subject has on an object.
#[instrument(skip(state))]
pub async fn list_handler(
    State(state): State<Arc<PermissionState>>,
    axum::extract::Query(query): axum::extract::Query<ListQuery>,
) -> impl IntoResponse {
    debug!(
        "List relations: {} on {}",
        query.subject, query.object
    );

    // Parse subject
    let subject = match Subject::parse(&query.subject) {
        Ok(s) => s,
        Err(e) => {
            warn!("Invalid subject format: {}", e);
            return permission_error_to_response(e).into_response();
        }
    };

    // Parse object
    let object = match Object::parse(&query.object) {
        Ok(o) => o,
        Err(e) => {
            warn!("Invalid object format: {}", e);
            return permission_error_to_response(e).into_response();
        }
    };

    // List relations
    match state
        .permission_service
        .list_relations(&subject, &object)
        .await
    {
        Ok(relations) => {
            debug!("Found {} relations", relations.len());
            (StatusCode::OK, Json(ListResponse { relations })).into_response()
        }
        Err(e) => {
            error!("Failed to list relations: {}", e);
            permission_error_to_response(e).into_response()
        }
    }
}

/// POST /v1/permissions/tuples
///
/// Write a new permission tuple.
#[instrument(skip(state))]
pub async fn write_tuple_handler(
    State(state): State<Arc<PermissionState>>,
    Json(request): Json<WriteTupleRequest>,
) -> impl IntoResponse {
    info!(
        "Write tuple: {}#{}@{}",
        request.object, request.relation, request.subject
    );

    // Parse object
    let object = match Object::parse(&request.object) {
        Ok(o) => o,
        Err(e) => {
            warn!("Invalid object format: {}", e);
            return permission_error_to_response(e).into_response();
        }
    };

    // Parse subject
    let subject = match Subject::parse(&request.subject) {
        Ok(s) => s,
        Err(e) => {
            warn!("Invalid subject format: {}", e);
            return permission_error_to_response(e).into_response();
        }
    };

    // Create tuple
    let tuple = Tuple::new(object, Relation::new(&request.relation), subject);
    let tuple_id = tuple.id.clone();
    let tuple_str = tuple.to_string();

    // Write tuple
    match state.permission_service.write_tuple(tuple).await {
        Ok(()) => {
            info!("Tuple written: {}", tuple_str);
            (
                StatusCode::CREATED,
                Json(WriteTupleResponse {
                    id: tuple_id,
                    tuple: tuple_str,
                }),
            )
                .into_response()
        }
        Err(e) => {
            error!("Failed to write tuple: {}", e);
            permission_error_to_response(e).into_response()
        }
    }
}

/// DELETE /v1/permissions/tuples
///
/// Delete a permission tuple.
#[instrument(skip(state))]
pub async fn delete_tuple_handler(
    State(state): State<Arc<PermissionState>>,
    Json(request): Json<DeleteTupleRequest>,
) -> impl IntoResponse {
    info!(
        "Delete tuple: {}#{}@{}",
        request.object, request.relation, request.subject
    );

    // Parse object
    let object = match Object::parse(&request.object) {
        Ok(o) => o,
        Err(e) => {
            warn!("Invalid object format: {}", e);
            return permission_error_to_response(e).into_response();
        }
    };

    // Parse subject
    let subject = match Subject::parse(&request.subject) {
        Ok(s) => s,
        Err(e) => {
            warn!("Invalid subject format: {}", e);
            return permission_error_to_response(e).into_response();
        }
    };

    // Create tuple (just for deletion matching)
    let tuple = Tuple::new(object, Relation::new(&request.relation), subject);

    // Delete tuple
    match state.permission_service.delete_tuple(&tuple).await {
        Ok(()) => {
            info!("Tuple deleted");
            StatusCode::NO_CONTENT.into_response()
        }
        Err(e) => {
            error!("Failed to delete tuple: {}", e);
            permission_error_to_response(e).into_response()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn create_test_permission_state() -> Arc<PermissionState> {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let db_pool = DatabasePool::new(config, pool_config).await.unwrap();

        // Run migrations
        let runner = MigrationRunner::global();
        runner.run(db_pool.global()).await.unwrap();

        let app_state = Arc::new(AppState::new(db_pool));
        Arc::new(PermissionState::new(app_state))
    }

    #[tokio::test]
    async fn test_check_permission_denied() {
        let permission_state = create_test_permission_state().await;
        let app = router(permission_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/check")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "subject": "user:did:plc:alice",
                            "permission": "delete",
                            "object": "waddle:test"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["allowed"], false);
    }

    #[tokio::test]
    async fn test_write_and_check_tuple() {
        let permission_state = create_test_permission_state().await;
        let app = router(permission_state);

        // Write a tuple
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/tuples")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "object": "waddle:test",
                            "relation": "owner",
                            "subject": "user:did:plc:alice"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert!(json["id"].is_string());
        assert_eq!(json["tuple"], "waddle:test#owner@user:did:plc:alice");

        // Check the permission
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/check")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "subject": "user:did:plc:alice",
                            "permission": "owner",
                            "object": "waddle:test"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["allowed"], true);
    }

    #[tokio::test]
    async fn test_computed_permission() {
        let permission_state = create_test_permission_state().await;
        let app = router(permission_state);

        // Write owner tuple
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/tuples")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "object": "waddle:test",
                            "relation": "owner",
                            "subject": "user:did:plc:alice"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);

        // Check computed "delete" permission (owner should have it)
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/check")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "subject": "user:did:plc:alice",
                            "permission": "delete",
                            "object": "waddle:test"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["allowed"], true);
    }

    #[tokio::test]
    async fn test_list_relations() {
        let permission_state = create_test_permission_state().await;
        let app = router(permission_state);

        // Write multiple tuples
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/tuples")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "object": "waddle:test",
                            "relation": "owner",
                            "subject": "user:did:plc:alice"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/tuples")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "object": "waddle:test",
                            "relation": "admin",
                            "subject": "user:did:plc:alice"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // List relations
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/v1/permissions/list?subject=user:did:plc:alice&object=waddle:test")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        let relations = json["relations"].as_array().unwrap();
        assert_eq!(relations.len(), 2);
        assert!(relations.contains(&serde_json::json!("owner")));
        assert!(relations.contains(&serde_json::json!("admin")));
    }

    #[tokio::test]
    async fn test_delete_tuple() {
        let permission_state = create_test_permission_state().await;
        let app = router(permission_state);

        // Write a tuple
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/tuples")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "object": "waddle:test",
                            "relation": "owner",
                            "subject": "user:did:plc:alice"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Delete the tuple
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/permissions/tuples")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "object": "waddle:test",
                            "relation": "owner",
                            "subject": "user:did:plc:alice"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);

        // Verify it's gone
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/check")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "subject": "user:did:plc:alice",
                            "permission": "owner",
                            "object": "waddle:test"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["allowed"], false);
    }

    #[tokio::test]
    async fn test_invalid_subject_format() {
        let permission_state = create_test_permission_state().await;
        let app = router(permission_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/check")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "subject": "invalid",
                            "permission": "view",
                            "object": "waddle:test"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["error"], "invalid_subject");
    }

    #[tokio::test]
    async fn test_invalid_object_format() {
        let permission_state = create_test_permission_state().await;
        let app = router(permission_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/check")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "subject": "user:did:plc:alice",
                            "permission": "view",
                            "object": "invalid"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["error"], "invalid_object");
    }

    #[tokio::test]
    async fn test_duplicate_tuple() {
        let permission_state = create_test_permission_state().await;
        let app = router(permission_state);

        // Write a tuple
        app.clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/tuples")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "object": "waddle:test",
                            "relation": "owner",
                            "subject": "user:did:plc:alice"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        // Try to write it again
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("POST")
                    .uri("/v1/permissions/tuples")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "object": "waddle:test",
                            "relation": "owner",
                            "subject": "user:did:plc:alice"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CONFLICT);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["error"], "tuple_exists");
    }

    #[tokio::test]
    async fn test_delete_nonexistent_tuple() {
        let permission_state = create_test_permission_state().await;
        let app = router(permission_state);

        let response = app
            .oneshot(
                Request::builder()
                    .method("DELETE")
                    .uri("/v1/permissions/tuples")
                    .header("Content-Type", "application/json")
                    .body(Body::from(
                        r#"{
                            "object": "waddle:test",
                            "relation": "owner",
                            "subject": "user:did:plc:alice"
                        }"#,
                    ))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["error"], "tuple_not_found");
    }
}
