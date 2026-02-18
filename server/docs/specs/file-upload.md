# File Upload Protocol Specification

## Overview

This document specifies the file upload protocol for Waddle Social, including chunked uploads, presigned URLs, and attachment processing.

## Upload Flow

### Simple Upload (< 5MB)

```
┌────────┐                     ┌────────────┐                    ┌─────────┐
│ Client │                     │   Waddle   │                    │   S3    │
└───┬────┘                     └─────┬──────┘                    └────┬────┘
    │                                │                                │
    │ 1. Request upload URL          │                                │
    │───────────────────────────────>│                                │
    │                                │                                │
    │ 2. Presigned URL               │                                │
    │<───────────────────────────────│                                │
    │                                │                                │
    │ 3. PUT file directly to S3     │                                │
    │───────────────────────────────────────────────────────────────>│
    │                                │                                │
    │ 4. Confirm upload              │                                │
    │───────────────────────────────>│                                │
    │                                │ 5. Verify & process            │
    │                                │───────────────────────────────>│
    │ 6. Attachment ready            │                                │
    │<───────────────────────────────│                                │
```

### Chunked Upload (> 5MB)

```
┌────────┐                     ┌────────────┐                    ┌─────────┐
│ Client │                     │   Waddle   │                    │   S3    │
└───┬────┘                     └─────┬──────┘                    └────┬────┘
    │                                │                                │
    │ 1. Initiate multipart upload   │                                │
    │───────────────────────────────>│                                │
    │                                │ 2. Create multipart            │
    │                                │───────────────────────────────>│
    │ 3. Upload ID + part URLs       │                                │
    │<───────────────────────────────│                                │
    │                                │                                │
    │ 4. PUT part 1                  │                                │
    │───────────────────────────────────────────────────────────────>│
    │ 5. ETag for part 1             │                                │
    │<───────────────────────────────────────────────────────────────│
    │                                │                                │
    │    ... repeat for all parts ...│                                │
    │                                │                                │
    │ 6. Complete upload (ETags)     │                                │
    │───────────────────────────────>│                                │
    │                                │ 7. Complete multipart          │
    │                                │───────────────────────────────>│
    │ 8. Attachment ready            │                                │
    │<───────────────────────────────│                                │
```

## API Endpoints

### Request Upload URL

```http
POST /v1/uploads HTTP/1.1
Authorization: Bearer <token>
Content-Type: application/json

{
  "filename": "screenshot.png",
  "content_type": "image/png",
  "size": 245678,
  "channel_id": "ch_general"  // For permission check
}
```

Response (simple upload):
```json
{
  "upload_id": "upload_abc123",
  "method": "simple",
  "upload_url": "https://s3.example.com/bucket/key?X-Amz-...",
  "expires_at": "2024-01-15T11:00:00Z",
  "max_size": 5242880
}
```

Response (multipart upload):
```json
{
  "upload_id": "upload_abc123",
  "method": "multipart",
  "s3_upload_id": "multipart_xyz",
  "part_size": 5242880,
  "total_parts": 3,
  "part_urls": [
    {
      "part_number": 1,
      "url": "https://s3.example.com/bucket/key?partNumber=1&uploadId=xyz&..."
    },
    {
      "part_number": 2,
      "url": "https://s3.example.com/bucket/key?partNumber=2&uploadId=xyz&..."
    },
    {
      "part_number": 3,
      "url": "https://s3.example.com/bucket/key?partNumber=3&uploadId=xyz&..."
    }
  ],
  "expires_at": "2024-01-15T12:00:00Z"
}
```

### Complete Multipart Upload

```http
POST /v1/uploads/upload_abc123/complete HTTP/1.1
Authorization: Bearer <token>
Content-Type: application/json

{
  "parts": [
    { "part_number": 1, "etag": "\"abc123\"" },
    { "part_number": 2, "etag": "\"def456\"" },
    { "part_number": 3, "etag": "\"ghi789\"" }
  ]
}
```

Response:
```json
{
  "attachment_id": "att_xyz789",
  "url": "https://cdn.waddle.social/attachments/xyz/screenshot.png",
  "thumbnail_url": "https://cdn.waddle.social/attachments/xyz/screenshot_thumb.png",
  "width": 1920,
  "height": 1080,
  "size": 245678
}
```

### Cancel Upload

```http
DELETE /v1/uploads/upload_abc123 HTTP/1.1
Authorization: Bearer <token>
```

### Get Upload Status

```http
GET /v1/uploads/upload_abc123 HTTP/1.1
Authorization: Bearer <token>
```

Response:
```json
{
  "upload_id": "upload_abc123",
  "status": "processing",  // pending, uploading, processing, complete, failed
  "progress": {
    "parts_uploaded": 2,
    "total_parts": 3,
    "bytes_uploaded": 10485760
  },
  "attachment": null  // Populated when complete
}
```

## File Types

### Supported Types

| Category | Extensions | Max Size | Processing |
|----------|------------|----------|------------|
| Images | jpg, jpeg, png, gif, webp | 10 MB | Thumbnail, resize |
| Videos | mp4, webm, mov | 100 MB | Thumbnail, transcode |
| Audio | mp3, ogg, wav, flac | 50 MB | Waveform |
| Documents | pdf, txt, md | 25 MB | Preview (PDF) |
| Archives | zip | 100 MB | List contents |

### Validation

```rust
fn validate_upload(request: &UploadRequest) -> Result<()> {
    // Check file extension
    let ext = Path::new(&request.filename)
        .extension()
        .and_then(|e| e.to_str())
        .ok_or(Error::InvalidFilename)?;

    let allowed = ALLOWED_EXTENSIONS.get(ext)
        .ok_or(Error::UnsupportedFileType)?;

    // Check content type matches
    if !allowed.content_types.contains(&request.content_type) {
        return Err(Error::ContentTypeMismatch);
    }

    // Check size limit
    if request.size > allowed.max_size {
        return Err(Error::FileTooLarge);
    }

    Ok(())
}
```

### Magic Number Validation

Server-side validation after upload:

```rust
async fn validate_file_content(key: &str) -> Result<()> {
    let head = s3_client.get_object()
        .bucket(&bucket)
        .key(key)
        .range("bytes=0-256")
        .send()
        .await?;

    let bytes = head.body.collect().await?.into_bytes();
    let detected = infer::get(&bytes)
        .ok_or(Error::UnknownFileType)?;

    if !ALLOWED_MIME_TYPES.contains(&detected.mime_type()) {
        // Delete the file
        s3_client.delete_object()
            .bucket(&bucket)
            .key(key)
            .send()
            .await?;

        return Err(Error::InvalidFileContent);
    }

    Ok(())
}
```

## Processing Pipeline

### Image Processing

```rust
async fn process_image(key: &str) -> Result<ImageMetadata> {
    let image_data = download_from_s3(key).await?;
    let img = image::load_from_memory(&image_data)?;

    let (width, height) = img.dimensions();

    // Generate thumbnail (max 400x400)
    let thumbnail = img.thumbnail(400, 400);
    let thumb_key = format!("{}_thumb", key);
    upload_to_s3(&thumb_key, &thumbnail.to_bytes()).await?;

    // Strip EXIF data for privacy
    let stripped = strip_exif(&image_data)?;
    upload_to_s3(key, &stripped).await?;

    Ok(ImageMetadata {
        width,
        height,
        thumbnail_key: thumb_key,
    })
}
```

### Video Processing

```rust
async fn process_video(key: &str) -> Result<VideoMetadata> {
    let local_path = download_to_temp(key).await?;

    // Extract metadata with ffprobe
    let probe = ffprobe(&local_path)?;
    let duration = probe.format.duration.parse::<f64>()?;
    let (width, height) = get_video_dimensions(&probe)?;

    // Generate thumbnail at 1 second
    let thumbnail_path = generate_thumbnail(&local_path, 1.0)?;
    let thumb_key = format!("{}_thumb.jpg", key);
    upload_to_s3(&thumb_key, &std::fs::read(&thumbnail_path)?).await?;

    // Transcode to web-friendly format if needed
    if needs_transcode(&probe) {
        let transcoded = transcode_video(&local_path)?;
        upload_to_s3(key, &std::fs::read(&transcoded)?).await?;
    }

    cleanup_temp_files().await?;

    Ok(VideoMetadata {
        width,
        height,
        duration: duration as u32,
        thumbnail_key: thumb_key,
    })
}
```

## Storage Structure

### S3 Key Format

```
attachments/
├── {waddle_id}/
│   ├── {channel_id}/
│   │   ├── {year}/{month}/
│   │   │   ├── {upload_id}_{filename}
│   │   │   └── {upload_id}_{filename}_thumb.jpg
```

Example:
```
attachments/waddle_123/ch_general/2024/01/upload_abc_screenshot.png
attachments/waddle_123/ch_general/2024/01/upload_abc_screenshot_thumb.png
```

### CDN URLs

Public URLs via CDN:
```
https://cdn.waddle.social/attachments/waddle_123/ch_general/2024/01/upload_abc_screenshot.png
```

### Presigned URLs

For private content or downloads:
```
https://s3.region.amazonaws.com/bucket/key?X-Amz-Algorithm=AWS4-HMAC-SHA256&X-Amz-Credential=...&X-Amz-Expires=3600&X-Amz-Signature=...
```

## Rate Limits

| Action | Limit | Window |
|--------|-------|--------|
| Upload requests | 10 | 1 minute |
| Active uploads | 5 | per user |
| Daily upload volume | 1 GB | per user |

## Cleanup

### Orphaned Uploads

Uploads not attached to messages within 24 hours:

```rust
async fn cleanup_orphaned_uploads() {
    let cutoff = Utc::now() - Duration::hours(24);

    let orphaned = db.query(
        "SELECT * FROM uploads
         WHERE status = 'complete'
         AND attachment_id IS NULL
         AND created_at < ?",
        [cutoff]
    ).await?;

    for upload in orphaned {
        s3_client.delete_object()
            .bucket(&bucket)
            .key(&upload.s3_key)
            .send()
            .await?;

        db.execute("DELETE FROM uploads WHERE id = ?", [upload.id]).await?;
    }
}
```

### Ephemeral Attachments

Attachments follow message TTL:

```rust
async fn cleanup_expired_attachments() {
    let expired = db.query(
        "SELECT a.* FROM attachments a
         JOIN messages m ON a.message_id = m.id
         WHERE m.expires_at IS NOT NULL
         AND m.expires_at < datetime('now')"
    ).await?;

    for attachment in expired {
        // Delete from S3
        s3_client.delete_object()
            .bucket(&bucket)
            .key(&attachment.s3_key)
            .send()
            .await?;

        // Delete thumbnail
        if let Some(thumb_key) = attachment.thumbnail_key {
            s3_client.delete_object()
                .bucket(&bucket)
                .key(&thumb_key)
                .send()
                .await?;
        }
    }
}
```

## Security

### Content Scanning

Optional integration with content scanning:

```rust
async fn scan_content(key: &str) -> Result<ScanResult> {
    // Download and scan with ClamAV or similar
    let scan_result = clamav_scan(key).await?;

    if scan_result.is_malware {
        // Delete immediately
        s3_client.delete_object()
            .bucket(&bucket)
            .key(key)
            .send()
            .await?;

        // Log incident
        log_security_event(SecurityEvent::MalwareDetected {
            key: key.to_string(),
            threat: scan_result.threat_name,
        });

        return Err(Error::MalwareDetected);
    }

    Ok(scan_result)
}
```

### Access Control

```rust
async fn can_access_attachment(user_did: &str, attachment: &Attachment) -> bool {
    // Check channel access
    permission_check(
        Subject::User(user_did),
        Permission::Read,
        Object::Channel(attachment.channel_id),
    ).await
}
```

## Related

- [ADR-0011: S3-Compatible Storage](../adrs/0011-self-hosted-storage.md)
- [RFC-0004: Rich Message Format](../rfcs/0004-message-format.md)
- [RFC-0005: Ephemeral Content](../rfcs/0005-ephemeral-content.md)
- [Spec: API Contracts](./api-contracts.md)
