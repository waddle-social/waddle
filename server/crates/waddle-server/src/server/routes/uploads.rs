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
use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::IntoResponse;
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::Serialize;
use std::sync::Arc;
use tracing::{debug, error, info, instrument, warn};

mod download;

use download::download_handler;

#[cfg(test)]
mod tests;

mod implementation;
use implementation::get_upload_slot;
pub(crate) use implementation::max_upload_size;
#[cfg(test)]
use implementation::max_upload_size_from_env_value;
pub use implementation::router;
use implementation::upload_error_to_response;
pub use implementation::upload_handler;
pub(crate) use implementation::upload_size_to_i64;
pub use implementation::ErrorResponse;
pub use implementation::UploadError;
pub use implementation::UploadState;
#[cfg(test)]
use implementation::DEFAULT_MAX_UPLOAD_SIZE;
pub(crate) use implementation::MAX_DATABASE_UPLOAD_SIZE;
