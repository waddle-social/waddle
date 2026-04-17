//! HTTP File Upload API Routes (XEP-0363)
//!
//! Provides HTTP endpoints for file upload and download:
//! - PUT /api/upload/:slot_id - Upload a file to a pre-allocated slot
//! - GET /api/files/:slot_id/:filename - Download an uploaded file
//!
//! Upload slots are created via the XMPP upload request flow (XEP-0363).
//! This module handles the HTTP portion of the upload/download process.

use crate::db::Database;
use crate::server::AppState;
use axum::{
    body::Bytes,
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use serde::Serialize;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

/// Extended application state for upload routes.
///
/// File storage is delegated to the `BlobStorage` trait on `AppState`,
/// which may be local filesystem or S3-compatible (e.g. Cloudflare R2).
pub struct UploadState {
    /// Core app state (includes blob storage backend)
    pub app_state: Arc<AppState>,
}

impl UploadState {
    /// Create new upload state
    pub fn new(app_state: Arc<AppState>) -> Self {
        Self { app_state }
    }

    /// Get the global database reference
    fn global_db(&self) -> &Database {
        self.app_state.db_pool.global()
    }
}

/// Create the uploads router
pub fn router(upload_state: Arc<UploadState>) -> Router {
    Router::new()
        // Upload endpoint (PUT /api/upload/:slot_id)
        .route(
            "/api/upload/:slot_id",
            put(upload_handler).options(upload_options_handler),
        )
        // Download endpoint (GET /api/files/:slot_id/:filename)
        .route("/api/files/:slot_id/:filename", get(download_handler))
        .with_state(upload_state)
}

/// OPTIONS /api/upload/:slot_id
///
/// Explicit preflight response for XEP-0363 CORS checks.
async fn upload_options_handler() -> impl IntoResponse {
    (
        StatusCode::NO_CONTENT,
        [
            (header::ACCESS_CONTROL_ALLOW_ORIGIN, "*"),
            (header::ACCESS_CONTROL_ALLOW_METHODS, "PUT, OPTIONS"),
            (
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                "Content-Type, Access-Control-Request-Method, Access-Control-Request-Headers, Origin",
            ),
        ],
    )
}

// === Response Types ===

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

/// Upload-specific error type
#[derive(Debug)]
pub enum UploadError {
    /// Slot not found
    SlotNotFound(String),
    /// Slot expired
    SlotExpired(String),
    /// Slot already used
    SlotAlreadyUsed(String),
    /// File size mismatch
    SizeMismatch { expected: i64, actual: i64 },
    /// Storage error
    Storage(String),
    /// Database error
    Database(String),
    /// File not found
    FileNotFound(String),
}

/// Convert UploadError to HTTP response
fn upload_error_to_response(err: UploadError) -> (StatusCode, Json<ErrorResponse>) {
    match err {
        UploadError::SlotNotFound(msg) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::new("slot_not_found", &msg)),
        ),
        UploadError::SlotExpired(msg) => (
            StatusCode::GONE,
            Json(ErrorResponse::new("slot_expired", &msg)),
        ),
        UploadError::SlotAlreadyUsed(msg) => (
            StatusCode::CONFLICT,
            Json(ErrorResponse::new("slot_already_used", &msg)),
        ),
        UploadError::SizeMismatch { expected, actual } => (
            StatusCode::BAD_REQUEST,
            Json(ErrorResponse::new(
                "size_mismatch",
                &format!(
                    "File size mismatch: expected {} bytes, got {} bytes",
                    expected, actual
                ),
            )),
        ),
        UploadError::Storage(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new("storage_error", &msg)),
        ),
        UploadError::Database(msg) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(ErrorResponse::new("database_error", &msg)),
        ),
        UploadError::FileNotFound(msg) => (
            StatusCode::NOT_FOUND,
            Json(ErrorResponse::new("file_not_found", &msg)),
        ),
    }
}

/// Upload slot information from database
#[derive(Debug)]
struct UploadSlotInfo {
    filename: String,
    size_bytes: i64,
    content_type: String,
    status: String,
    storage_key: Option<String>,
    expires_at: String,
}

/// Fetch upload slot from database
async fn get_upload_slot(db: &Database, slot_id: &str) -> Result<Option<UploadSlotInfo>, String> {
    let query = r#"
        SELECT id, filename, size_bytes, content_type, status, storage_key, expires_at
        FROM upload_slots
        WHERE id = ?
    "#;

    let conn = db.guard().await.map_err(|e| e.to_string())?;
    let mut rows = conn
        .query(query, libsql::params![slot_id])
        .await
        .map_err(|e| format!("Failed to query upload slot: {}", e))?;

    let row = rows
        .next()
        .await
        .map_err(|e| format!("Failed to read slot row: {}", e))?;

    match row {
        Some(row) => {
            let filename: String = row
                .get(1)
                .map_err(|e| format!("Failed to get filename: {}", e))?;
            let size_bytes: i64 = row
                .get(2)
                .map_err(|e| format!("Failed to get size_bytes: {}", e))?;
            let content_type: String = row
                .get(3)
                .map_err(|e| format!("Failed to get content_type: {}", e))?;
            let status: String = row
                .get(4)
                .map_err(|e| format!("Failed to get status: {}", e))?;
            let storage_key: Option<String> = row.get(5).ok();
            let expires_at: String = row
                .get(6)
                .map_err(|e| format!("Failed to get expires_at: {}", e))?;

            Ok(Some(UploadSlotInfo {
                filename,
                size_bytes,
                content_type,
                status,
                storage_key,
                expires_at,
            }))
        }
        None => Ok(None),
    }
}

/// Update slot status to 'uploaded' and set storage_key
async fn mark_slot_uploaded(db: &Database, slot_id: &str, storage_key: &str) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    let query = r#"
        UPDATE upload_slots
        SET status = 'uploaded', storage_key = ?, uploaded_at = ?
        WHERE id = ?
    "#;

    let conn = db.guard().await.map_err(|e| e.to_string())?;
    conn.execute(query, libsql::params![storage_key, now, slot_id])
        .await
        .map_err(|e| format!("Failed to update slot status: {}", e))?;

    Ok(())
}

/// Check if slot has expired
fn is_slot_expired(expires_at: &str) -> bool {
    match chrono::DateTime::parse_from_rfc3339(expires_at) {
        Ok(expiry) => chrono::Utc::now() > expiry,
        Err(_) => {
            // If we can't parse the expiry, treat as expired for safety
            warn!("Failed to parse slot expiry time: {}", expires_at);
            true
        }
    }
}

// === Handlers ===

/// PUT /api/upload/:slot_id
///
/// Upload a file to a pre-allocated slot. The slot must:
/// - Exist in the database
/// - Have status 'pending'
/// - Not be expired
/// - Match the Content-Length header with expected size
#[instrument(skip(state, headers, body))]
pub async fn upload_handler(
    State(state): State<Arc<UploadState>>,
    Path(slot_id): Path<String>,
    headers: HeaderMap,
    body: Bytes,
) -> impl IntoResponse {
    info!("Processing upload for slot: {}", slot_id);

    // Get upload slot from database
    let slot = match get_upload_slot(state.global_db(), &slot_id).await {
        Ok(Some(slot)) => slot,
        Ok(None) => {
            warn!("Upload slot not found: {}", slot_id);
            return upload_error_to_response(UploadError::SlotNotFound(format!(
                "Upload slot '{}' not found",
                slot_id
            )))
            .into_response();
        }
        Err(err) => {
            error!("Database error fetching slot: {}", err);
            return upload_error_to_response(UploadError::Database(err)).into_response();
        }
    };

    // Check slot status
    if slot.status == "uploaded" {
        warn!("Slot already used: {}", slot_id);
        return upload_error_to_response(UploadError::SlotAlreadyUsed(format!(
            "Upload slot '{}' has already been used",
            slot_id
        )))
        .into_response();
    }

    if slot.status != "pending" {
        warn!("Invalid slot status: {} for slot {}", slot.status, slot_id);
        return upload_error_to_response(UploadError::SlotNotFound(format!(
            "Upload slot '{}' is not in pending state",
            slot_id
        )))
        .into_response();
    }

    // Check expiry
    if is_slot_expired(&slot.expires_at) {
        warn!("Slot expired: {}", slot_id);
        return upload_error_to_response(UploadError::SlotExpired(format!(
            "Upload slot '{}' has expired",
            slot_id
        )))
        .into_response();
    }

    // Get actual body size
    let body_size = body.len() as i64;

    // Get Content-Length from headers for validation (if provided)
    if let Some(content_length) = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<i64>().ok())
    {
        // Validate Content-Length header matches expected size
        if content_length != slot.size_bytes {
            warn!(
                "Content-Length mismatch for slot {}: expected {}, got {}",
                slot_id, slot.size_bytes, content_length
            );
            return upload_error_to_response(UploadError::SizeMismatch {
                expected: slot.size_bytes,
                actual: content_length,
            })
            .into_response();
        }
    }

    // Validate actual body size matches expected
    if body_size != slot.size_bytes {
        warn!(
            "Size mismatch for slot {}: expected {}, got {}",
            slot_id, slot.size_bytes, body_size
        );
        return upload_error_to_response(UploadError::SizeMismatch {
            expected: slot.size_bytes,
            actual: body_size,
        })
        .into_response();
    }

    // Optionally validate Content-Type (but don't be too strict)
    if let Some(content_type) = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
    {
        // Only validate if it's not the default octet-stream
        if slot.content_type != "application/octet-stream"
            && content_type != slot.content_type
            && !content_type.starts_with(&slot.content_type)
        {
            debug!(
                "Content-Type mismatch for slot {}: expected '{}', got '{}'",
                slot_id, slot.content_type, content_type
            );
            // Don't fail on content-type mismatch, just log it
        }
    }

    // Store via blob storage backend (local filesystem or S3/R2)
    let storage_key = format!("{}/{}", slot_id, slot.filename);

    if let Err(err) = state
        .app_state
        .blob_storage
        .put(&storage_key, body.clone(), &slot.content_type)
        .await
    {
        error!("Failed to store blob: {}", err);
        return upload_error_to_response(UploadError::Storage(format!(
            "Failed to store file: {}",
            err
        )))
        .into_response();
    }

    // Update slot status in database
    if let Err(err) = mark_slot_uploaded(state.global_db(), &slot_id, &storage_key).await {
        error!("Failed to update slot status: {}", err);
        warn!("File uploaded but database status update failed");
    }

    info!(
        "Upload complete: {} bytes stored at {}",
        body_size, storage_key
    );

    StatusCode::CREATED.into_response()
}

/// GET /api/files/:slot_id/:filename
///
/// Download an uploaded file. The slot must:
/// - Exist in the database
/// - Have status 'uploaded'
/// - Have a valid storage_key
#[instrument(skip(state))]
pub async fn download_handler(
    State(state): State<Arc<UploadState>>,
    Path((slot_id, filename)): Path<(String, String)>,
) -> impl IntoResponse {
    debug!(
        "Download request for slot: {}, filename: {}",
        slot_id, filename
    );

    // Get upload slot from database
    let slot = match get_upload_slot(state.global_db(), &slot_id).await {
        Ok(Some(slot)) => slot,
        Ok(None) => {
            warn!("Download slot not found: {}", slot_id);
            return upload_error_to_response(UploadError::FileNotFound(format!(
                "File not found for slot '{}'",
                slot_id
            )))
            .into_response();
        }
        Err(err) => {
            error!("Database error fetching slot: {}", err);
            return upload_error_to_response(UploadError::Database(err)).into_response();
        }
    };

    // Verify status is uploaded
    if slot.status != "uploaded" {
        warn!(
            "File not yet uploaded for slot {}: status is '{}'",
            slot_id, slot.status
        );
        return upload_error_to_response(UploadError::FileNotFound(format!(
            "File not yet uploaded for slot '{}'",
            slot_id
        )))
        .into_response();
    }

    // Verify filename matches (basic security check)
    if slot.filename != filename {
        warn!(
            "Filename mismatch for slot {}: expected '{}', got '{}'",
            slot_id, slot.filename, filename
        );
        return upload_error_to_response(UploadError::FileNotFound(format!(
            "File '{}' not found",
            filename
        )))
        .into_response();
    }

    // Get storage key
    let storage_key = match &slot.storage_key {
        Some(key) => key.clone(),
        None => {
            error!("Slot {} has no storage_key despite being uploaded", slot_id);
            return upload_error_to_response(UploadError::Storage(
                "File storage key missing".to_string(),
            ))
            .into_response();
        }
    };

    // Retrieve from blob storage backend
    let (file_contents, blob_meta) = match state.app_state.blob_storage.get(&storage_key).await {
        Ok(result) => result,
        Err(crate::storage::StorageError::NotFound(_)) => {
            error!("File not found in storage: {}", storage_key);
            return upload_error_to_response(UploadError::FileNotFound(format!(
                "File '{}' not found on server",
                filename
            )))
            .into_response();
        }
        Err(err) => {
            error!("Failed to read from storage: {}", err);
            return upload_error_to_response(UploadError::Storage(format!(
                "Failed to read file: {}",
                err
            )))
            .into_response();
        }
    };

    // Build response with appropriate headers
    let mut headers = HeaderMap::new();

    // Use content-type from blob metadata (authoritative), fall back to DB
    let content_type = if blob_meta.content_type != "application/octet-stream" {
        blob_meta.content_type
    } else {
        slot.content_type
    };
    if let Ok(ct) = content_type.parse() {
        headers.insert(header::CONTENT_TYPE, ct);
    }

    if let Ok(content_length) = file_contents.len().to_string().parse() {
        headers.insert(header::CONTENT_LENGTH, content_length);
    }

    let disposition = format!("inline; filename=\"{}\"", slot.filename);
    if let Ok(disp) = disposition.parse() {
        headers.insert(header::CONTENT_DISPOSITION, disp);
    }

    if let Ok(cache) = "public, max-age=31536000, immutable".parse() {
        headers.insert(header::CACHE_CONTROL, cache);
    }

    info!(
        "Serving file: {} ({} bytes)",
        slot.filename,
        file_contents.len()
    );

    (StatusCode::OK, headers, file_contents).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use axum::body::Body;
    use axum::http::Request;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    async fn create_test_upload_state() -> (Arc<UploadState>, std::path::PathBuf) {
        let config = DatabaseConfig::default();
        let pool_config = PoolConfig::default();
        let db_pool = DatabasePool::new(config, pool_config).await.unwrap();

        // Run migrations
        let runner = MigrationRunner::global();
        runner.run(db_pool.global()).await.unwrap();

        // Create temp dir for local blob storage
        let upload_dir = std::env::temp_dir().join(format!("waddle-test-{}", uuid::Uuid::new_v4()));
        std::fs::create_dir_all(&upload_dir).unwrap();

        let blob_storage: Arc<dyn crate::storage::BlobStorage> =
            Arc::new(crate::storage::LocalStorage::new(upload_dir.clone()));

        let app_state = Arc::new(AppState::new_with_deps(Arc::new(db_pool), blob_storage));

        (Arc::new(UploadState::new(app_state)), upload_dir)
    }

    async fn create_test_slot(
        state: &UploadState,
        slot_id: &str,
        filename: &str,
        size: i64,
        content_type: &str,
    ) {
        let expires_at = (chrono::Utc::now() + chrono::Duration::minutes(15)).to_rfc3339();
        let db = state.global_db();

        let query = r#"
            INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, status, expires_at)
            VALUES (?, 'test@example.com', ?, ?, ?, 'pending', ?)
        "#;

        let conn = db.guard().await.unwrap();
        conn.execute(
            query,
            libsql::params![slot_id, filename, size, content_type, expires_at],
        )
        .await
        .unwrap();
    }

    async fn create_expired_slot(state: &UploadState, slot_id: &str) {
        // Create a slot that expired 1 hour ago
        let expires_at = (chrono::Utc::now() - chrono::Duration::hours(1)).to_rfc3339();
        let db = state.global_db();

        let query = r#"
            INSERT INTO upload_slots (id, requester_jid, filename, size_bytes, content_type, status, expires_at)
            VALUES (?, 'test@example.com', 'expired.txt', 100, 'text/plain', 'pending', ?)
        "#;

        let conn = db.guard().await.unwrap();
        conn.execute(query, libsql::params![slot_id, expires_at])
            .await
            .unwrap();
    }

    #[tokio::test]
    async fn test_upload_to_valid_slot() {
        let (state, upload_dir) = create_test_upload_state().await;
        let slot_id = uuid::Uuid::new_v4().to_string();
        let file_content = b"Hello, World!";

        create_test_slot(
            &state,
            &slot_id,
            "test.txt",
            file_content.len() as i64,
            "text/plain",
        )
        .await;

        let app = router(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/upload/{}", slot_id))
                    .header("Content-Type", "text/plain")
                    .header("Content-Length", file_content.len().to_string())
                    .body(Body::from(file_content.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::CREATED);

        // Verify file was written
        let file_path = upload_dir.join(&slot_id).join("test.txt");
        assert!(file_path.exists());

        let saved_content = std::fs::read(&file_path).unwrap();
        assert_eq!(saved_content, file_content);

        // Verify slot status was updated
        let slot = get_upload_slot(state.global_db(), &slot_id)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(slot.status, "uploaded");
        assert!(slot.storage_key.is_some());

        // Cleanup
        std::fs::remove_dir_all(&upload_dir).ok();
    }

    #[tokio::test]
    async fn test_upload_options_cors_headers() {
        let (state, upload_dir) = create_test_upload_state().await;
        let slot_id = uuid::Uuid::new_v4().to_string();
        let app = router(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method("OPTIONS")
                    .uri(format!("/api/upload/{}", slot_id))
                    .header("Origin", "https://compliance.conversations.im")
                    .header("Access-Control-Request-Method", "PUT")
                    .header("Access-Control-Request-Headers", "content-type")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(
            response
                .headers()
                .get("access-control-allow-origin")
                .unwrap()
                .to_str()
                .unwrap(),
            "*"
        );
        assert!(response
            .headers()
            .contains_key("access-control-allow-methods"));
        assert!(response
            .headers()
            .contains_key("access-control-allow-headers"));

        std::fs::remove_dir_all(&upload_dir).ok();
    }

    #[tokio::test]
    async fn test_upload_to_nonexistent_slot() {
        let (state, upload_dir) = create_test_upload_state().await;
        let app = router(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri("/api/upload/nonexistent-slot")
                    .header("Content-Type", "text/plain")
                    .header("Content-Length", "5")
                    .body(Body::from("hello"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "slot_not_found");

        // Cleanup
        std::fs::remove_dir_all(&upload_dir).ok();
    }

    #[tokio::test]
    async fn test_upload_to_expired_slot() {
        let (state, upload_dir) = create_test_upload_state().await;
        let slot_id = uuid::Uuid::new_v4().to_string();

        create_expired_slot(&state, &slot_id).await;

        let app = router(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/upload/{}", slot_id))
                    .header("Content-Type", "text/plain")
                    .header("Content-Length", "5")
                    .body(Body::from("hello"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::GONE);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "slot_expired");

        // Cleanup
        std::fs::remove_dir_all(&upload_dir).ok();
    }

    #[tokio::test]
    async fn test_upload_size_mismatch() {
        let (state, upload_dir) = create_test_upload_state().await;
        let slot_id = uuid::Uuid::new_v4().to_string();

        // Create slot expecting 100 bytes
        create_test_slot(&state, &slot_id, "test.txt", 100, "text/plain").await;

        let app = router(state.clone());

        // Try to upload only 5 bytes
        let response = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/upload/{}", slot_id))
                    .header("Content-Type", "text/plain")
                    .header("Content-Length", "5")
                    .body(Body::from("hello"))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "size_mismatch");

        // Cleanup
        std::fs::remove_dir_all(&upload_dir).ok();
    }

    #[tokio::test]
    async fn test_download_uploaded_file() {
        let (state, upload_dir) = create_test_upload_state().await;
        let slot_id = uuid::Uuid::new_v4().to_string();
        let file_content = b"Test file content";

        // Create and upload a file
        create_test_slot(
            &state,
            &slot_id,
            "download-test.txt",
            file_content.len() as i64,
            "text/plain",
        )
        .await;

        let app = router(state.clone());

        // Upload the file first
        let upload_response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/upload/{}", slot_id))
                    .header("Content-Type", "text/plain")
                    .header("Content-Length", file_content.len().to_string())
                    .body(Body::from(file_content.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(upload_response.status(), StatusCode::CREATED);

        // Now download it
        let download_response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/files/{}/download-test.txt", slot_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(download_response.status(), StatusCode::OK);

        // Check headers
        let headers = download_response.headers();
        assert_eq!(
            headers.get("content-type").unwrap().to_str().unwrap(),
            "text/plain"
        );

        // Check content
        let body = download_response
            .into_body()
            .collect()
            .await
            .unwrap()
            .to_bytes();
        assert_eq!(body.as_ref(), file_content);

        // Cleanup
        std::fs::remove_dir_all(&upload_dir).ok();
    }

    #[tokio::test]
    async fn test_download_nonexistent_file() {
        let (state, upload_dir) = create_test_upload_state().await;
        let app = router(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri("/api/files/nonexistent/file.txt")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        // Cleanup
        std::fs::remove_dir_all(&upload_dir).ok();
    }

    #[tokio::test]
    async fn test_download_pending_slot() {
        let (state, upload_dir) = create_test_upload_state().await;
        let slot_id = uuid::Uuid::new_v4().to_string();

        // Create a slot but don't upload to it
        create_test_slot(&state, &slot_id, "pending.txt", 100, "text/plain").await;

        let app = router(state.clone());

        let response = app
            .oneshot(
                Request::builder()
                    .method("GET")
                    .uri(format!("/api/files/{}/pending.txt", slot_id))
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NOT_FOUND);

        let body = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "file_not_found");
        assert!(json["message"]
            .as_str()
            .unwrap()
            .contains("not yet uploaded"));

        // Cleanup
        std::fs::remove_dir_all(&upload_dir).ok();
    }

    #[tokio::test]
    async fn test_double_upload_fails() {
        let (state, upload_dir) = create_test_upload_state().await;
        let slot_id = uuid::Uuid::new_v4().to_string();
        let file_content = b"Hello, World!";

        create_test_slot(
            &state,
            &slot_id,
            "test.txt",
            file_content.len() as i64,
            "text/plain",
        )
        .await;

        let app = router(state.clone());

        // First upload should succeed
        let response1 = app
            .clone()
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/upload/{}", slot_id))
                    .header("Content-Type", "text/plain")
                    .header("Content-Length", file_content.len().to_string())
                    .body(Body::from(file_content.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response1.status(), StatusCode::CREATED);

        // Second upload should fail
        let response2 = app
            .oneshot(
                Request::builder()
                    .method("PUT")
                    .uri(format!("/api/upload/{}", slot_id))
                    .header("Content-Type", "text/plain")
                    .header("Content-Length", file_content.len().to_string())
                    .body(Body::from(file_content.to_vec()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response2.status(), StatusCode::CONFLICT);

        let body = response2.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["error"], "slot_already_used");

        // Cleanup
        std::fs::remove_dir_all(&upload_dir).ok();
    }
}
