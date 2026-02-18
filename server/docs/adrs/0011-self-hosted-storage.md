# ADR-0011: S3-Compatible File Storage

## Status

Accepted

## Context

Waddle Social needs file storage for:
- Message attachments (images, documents, videos)
- User avatars and Waddle icons
- Media for Watch Together, screen sharing, streaming

Requirements:
- Self-hostable (no vendor lock-in)
- Scalable to large file counts
- CDN-friendly for global distribution
- Resumable uploads for large files

We evaluated:
- **Local Filesystem**: Simple but doesn't scale; no CDN integration
- **S3 (AWS)**: Industry standard API, but proprietary
- **S3-Compatible**: MinIO, Cloudflare R2, Backblaze B2; same API, choice of provider
- **Object Storage Abstraction**: `object_store` crate supports multiple backends

## Decision

We will use **S3-compatible object storage** with configurable backends.

## Consequences

### Positive

- **Self-Hostable**: MinIO, SeaweedFS can be self-hosted
- **Cloud Options**: R2, B2, S3 available for managed hosting
- **Standard API**: S3 API is well-documented, widely supported
- **CDN Integration**: Easy to front with Cloudflare, CloudFront
- **Rust Support**: `aws-sdk-s3` and `object_store` crates available
- **Presigned URLs**: Direct client uploads reduce server load

### Negative

- **Configuration Complexity**: Users must provision storage separately
- **Cost Variability**: Egress fees vary significantly by provider
- **Consistency Model**: S3's eventual consistency can surprise developers

### Neutral

- **No Built-in Storage**: Intentional; keeps core application stateless

## Implementation Notes

- Use `object_store` crate for backend abstraction
- Environment variables for endpoint, bucket, credentials
- Presigned URLs for direct client uploads/downloads
- Lifecycle policies for ephemeral content cleanup

## Related

- [RFC-0004: Rich Message Format](../rfcs/0004-message-format.md) (attachments)
- [RFC-0005: Ephemeral Content](../rfcs/0005-ephemeral-content.md) (TTL cleanup)
- [Spec: File Upload Protocol](../specs/file-upload.md)
