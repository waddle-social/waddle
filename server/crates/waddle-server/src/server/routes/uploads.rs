//! HTTP File Upload API Routes (XEP-0363)
//!
//! Provides HTTP endpoints for file upload and download:
//! - PUT /api/upload/{slot_id} - Upload a file to a pre-allocated slot
//! - GET /api/files/{slot_id}/{filename} - Download an uploaded file
//!
//! Upload slots are created via the XMPP upload request flow (XEP-0363).
//! This module handles the HTTP portion of the upload/download process.

use crate::db::actor::{DbActor, DbExecute, DbQueryOne};
#[cfg(test)]
use crate::db::Database;
use crate::db::{row_value, ValueExt};
use crate::server::AppState;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, Path, State},
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::{get, put},
    Json, Router,
};
use serde::Serialize;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

mod download;

use download::download_handler;

const DEFAULT_MAX_UPLOAD_SIZE: u64 = 10 * 1024 * 1024;
pub(crate) const MAX_DATABASE_UPLOAD_SIZE: u64 = i64::MAX as u64;

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
    #[cfg(test)]
    fn global_db(&self) -> &Database {
        self.app_state.db_pool.global()
    }

    fn global_actor(&self) -> kameo::actor::ActorRef<DbActor> {
        self.app_state.db_pool.global_actor().clone()
    }
}

pub(crate) fn max_upload_size() -> u64 {
    max_upload_size_from_env_value(std::env::var("WADDLE_MAX_UPLOAD_SIZE").ok().as_deref())
}

pub(crate) fn upload_size_to_i64(size: u64) -> Option<i64> {
    i64::try_from(size).ok()
}

fn max_upload_size_from_env_value(raw: Option<&str>) -> u64 {
    raw.and_then(|s| s.parse::<u64>().ok())
        .map(|size| size.min(MAX_DATABASE_UPLOAD_SIZE))
        .unwrap_or(DEFAULT_MAX_UPLOAD_SIZE)
}

fn upload_body_limit() -> DefaultBodyLimit {
    // The upload handler extracts the request body as `Bytes`, so Axum's
    // default 2 MiB limit would reject larger uploads before our slot/size
    // validation runs. Keep the HTTP body cap aligned with the XMPP slot cap.
    let limit = usize::try_from(max_upload_size()).unwrap_or(usize::MAX);
    DefaultBodyLimit::max(limit)
}

/// Create the uploads router
pub fn router(upload_state: Arc<UploadState>) -> Router {
    Router::new()
        // Upload endpoint (PUT /api/upload/{slot_id})
        .route(
            "/api/upload/{slot_id}",
            put(upload_handler).options(upload_options_handler),
        )
        // Download endpoint (GET /api/files/{slot_id}/{filename})
        .route("/api/files/{slot_id}/{filename}", get(download_handler))
        .layer(upload_body_limit())
        .with_state(upload_state)
}

/// OPTIONS /api/upload/{slot_id}
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
async fn get_upload_slot(
    actor: kameo::actor::ActorRef<DbActor>,
    slot_id: &str,
) -> Result<Option<UploadSlotInfo>, String> {
    let query = r#"
        SELECT id, filename, size_bytes, content_type, status, storage_key, expires_at
        FROM upload_slots
        WHERE id = ?
    "#;

    let row = actor
        .ask(DbQueryOne {
            sql: query.to_string(),
            params: vec![slot_id.into()],
        })
        .await
        .map_err(|e| format!("Failed to query upload slot: {}", e))?;

    match row {
        Some(row) => {
            let filename = row_value(&row, 1)
                .and_then(ValueExt::as_string)
                .map_err(|e| format!("Failed to get filename: {}", e))?;
            let size_bytes =
                match row_value(&row, 2).map_err(|e| format!("Failed to get size_bytes: {}", e))? {
                    crate::db::Value::Integer(value) => *value,
                    other => {
                        return Err(format!(
                            "Failed to get size_bytes: unexpected value {:?}",
                            other
                        ));
                    }
                };
            let content_type = row_value(&row, 3)
                .and_then(ValueExt::as_string)
                .map_err(|e| format!("Failed to get content_type: {}", e))?;
            let status = row_value(&row, 4)
                .and_then(ValueExt::as_string)
                .map_err(|e| format!("Failed to get status: {}", e))?;
            let storage_key = row_value(&row, 5)
                .and_then(ValueExt::as_optional_string)
                .map_err(|e| format!("Failed to get storage_key: {}", e))?;
            let expires_at = row_value(&row, 6)
                .and_then(ValueExt::as_string)
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
async fn mark_slot_uploaded(
    actor: kameo::actor::ActorRef<DbActor>,
    slot_id: &str,
    storage_key: &str,
) -> Result<(), String> {
    let now = chrono::Utc::now().to_rfc3339();
    let query = r#"
        UPDATE upload_slots
        SET status = 'uploaded', storage_key = ?, uploaded_at = ?
        WHERE id = ?
    "#;

    actor
        .ask(DbExecute {
            sql: query.to_string(),
            params: vec![storage_key.into(), now.into(), slot_id.into()],
        })
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

/// PUT /api/upload/{slot_id}
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
    let slot = match get_upload_slot(state.global_actor(), &slot_id).await {
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
    if let Err(err) = mark_slot_uploaded(state.global_actor(), &slot_id, &storage_key).await {
        error!("Failed to update slot status: {}", err);
        warn!("File uploaded but database status update failed");
    }

    info!(
        "Upload complete: {} bytes stored at {}",
        body_size, storage_key
    );

    StatusCode::CREATED.into_response()
}

#[cfg(test)]
mod tests;
