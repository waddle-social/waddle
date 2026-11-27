use super::{
    debug, error, get_upload_slot, header, info, instrument, upload_error_to_response, warn, Arc,
    Extension, HeaderMap, IntoResponse, Path, ResolvedTenant, State, StatusCode, UploadError,
    UploadState,
};

/// GET /api/files/{slot_id}/{filename}
///
/// Download an uploaded file. The slot must:
/// - Exist in the database
/// - Have status 'uploaded'
/// - Have a valid storage_key
#[instrument(skip(state, tenant))]
pub(super) async fn download_handler(
    State(state): State<Arc<UploadState>>,
    tenant: Option<Extension<ResolvedTenant>>,
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

    if let Some(Extension(tenant)) = tenant {
        if !tenant.owns_jid_str(&slot.requester_jid) {
            warn!(
                slot_id = %slot_id,
                tenant = %tenant.tenant_id,
                "Rejecting cross-tenant file download"
            );
            return upload_error_to_response(UploadError::Forbidden(
                "File does not belong to this tenant".to_string(),
            ))
            .into_response();
        }
    }

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
    // Use content-type from blob metadata (authoritative), fall back to DB
    let content_type = if blob_meta.content_type != "application/octet-stream" {
        blob_meta.content_type
    } else {
        slot.content_type
    };

    let mut headers = download_headers(&content_type, &slot.filename);

    if let Ok(content_length) = file_contents.len().to_string().parse() {
        headers.insert(header::CONTENT_LENGTH, content_length);
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

/// Media types that are safe to render inline from the server origin.
/// `text/plain` is inert because every response also carries `nosniff`.
///
/// Anything else (HTML, SVG, XML, scripts, PDF, unknown types) is forced to
/// download so user-supplied content can never execute in the server origin
/// (XEP-0363 security considerations: isolate GET content, use CSP).
fn is_safe_inline_type(essence: &str) -> bool {
    matches!(
        essence,
        "image/png"
            | "image/jpeg"
            | "image/gif"
            | "image/webp"
            | "image/avif"
            | "image/bmp"
            | "text/plain"
    ) || essence.starts_with("audio/")
        || essence.starts_with("video/")
}

fn is_plain_filename_byte(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_')
}

/// RFC 6266 Content-Disposition value with an ASCII fallback and `filename*`.
fn content_disposition(kind: &str, filename: &str) -> String {
    let fallback: String = filename
        .chars()
        .map(|c| {
            if c.is_ascii() && is_plain_filename_byte(c as u8) {
                c
            } else {
                '_'
            }
        })
        .collect();
    let mut encoded = String::with_capacity(filename.len());
    for b in filename.bytes() {
        if is_plain_filename_byte(b) {
            encoded.push(char::from(b));
        } else {
            encoded.push_str(&format!("%{b:02X}"));
        }
    }
    format!("{kind}; filename=\"{fallback}\"; filename*=UTF-8''{encoded}")
}

/// Build the security-relevant response headers for a file download.
///
/// Every response carries `nosniff` and a locked-down CSP. Only an allowlist
/// of passive media types is served inline with its own Content-Type; every
/// other type is sent as `application/octet-stream` with `attachment`.
pub(super) fn download_headers(content_type: &str, filename: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    let essence = content_type
        .split(';')
        .next()
        .unwrap_or("")
        .trim()
        .to_ascii_lowercase();
    let inline = is_safe_inline_type(&essence);

    let served_type = if inline {
        content_type
    } else {
        "application/octet-stream"
    };
    headers.insert(
        header::CONTENT_TYPE,
        served_type
            .parse()
            .unwrap_or_else(|_| header::HeaderValue::from_static("application/octet-stream")),
    );
    let kind = if inline { "inline" } else { "attachment" };
    if let Ok(disp) = content_disposition(kind, filename).parse() {
        headers.insert(header::CONTENT_DISPOSITION, disp);
    }
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        header::HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        header::HeaderValue::from_static("default-src 'none'; frame-ancestors 'none'; sandbox"),
    );
    headers
}
