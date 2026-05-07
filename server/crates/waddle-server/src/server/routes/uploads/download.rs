use super::*;

/// GET /api/files/:slot_id/:filename
///
/// Download an uploaded file. The slot must:
/// - Exist in the database
/// - Have status 'uploaded'
/// - Have a valid storage_key
#[instrument(skip(state))]
pub(super) async fn download_handler(
    State(state): State<Arc<UploadState>>,
    Path((slot_id, filename)): Path<(String, String)>,
) -> impl IntoResponse {
    debug!(
        "Download request for slot: {}, filename: {}",
        slot_id, filename
    );

    // Get upload slot from database
    let slot = match get_upload_slot(state.global_actor(), &slot_id).await {
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
