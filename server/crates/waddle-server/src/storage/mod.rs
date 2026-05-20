//! Blob storage abstraction for file uploads (XEP-0363).
//!
//! Provides a `BlobStorage` trait with two implementations:
//! - `LocalStorage`: stores files on the local filesystem
//! - `S3Storage`: stores files in an S3-compatible bucket (e.g. Cloudflare R2)
//!
//! The active backend is selected by environment variables at startup via
//! `build_blob_storage()`.

use std::future::Future;
use std::path::PathBuf;
use std::pin::Pin;
use std::sync::Arc;

use object_store::aws::AmazonS3Builder;
use object_store::{ObjectStore, ObjectStoreExt};
use tracing::{debug, info};

/// Errors returned by blob storage operations.
#[derive(Debug, thiserror::Error)]
pub enum StorageError {
    #[error("blob not found: {0}")]
    NotFound(String),
    #[error("storage error: {0}")]
    Internal(String),
}

/// Metadata returned alongside blob data on retrieval.
pub struct BlobMeta {
    pub content_type: String,
}

/// A boxed, Send future — used for dyn-compatible async trait methods.
type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Trait abstracting binary object storage.
///
/// Uses boxed futures for dyn-compatibility so the trait can be used as
/// `Arc<dyn BlobStorage>`.
pub trait BlobStorage: Send + Sync {
    /// Store bytes at the given key with the specified content type.
    fn put(
        &self,
        key: &str,
        data: bytes::Bytes,
        content_type: &str,
    ) -> BoxFuture<'_, Result<(), StorageError>>;

    /// Retrieve bytes and metadata for the given key.
    fn get(&self, key: &str) -> BoxFuture<'_, Result<(bytes::Bytes, BlobMeta), StorageError>>;
}

// ---------------------------------------------------------------------------
// LocalStorage — filesystem backend
// ---------------------------------------------------------------------------

/// Stores blobs on the local filesystem under a base directory.
///
/// Layout: `{base_dir}/{key}` for the data, `{base_dir}/{key}.meta` for the
/// content-type sidecar (a plain text file containing just the MIME string).
pub struct LocalStorage {
    base_dir: PathBuf,
}

impl LocalStorage {
    pub fn new(base_dir: PathBuf) -> Self {
        Self { base_dir }
    }

    fn data_path(&self, key: &str) -> PathBuf {
        self.base_dir.join(key)
    }

    fn meta_path(&self, key: &str) -> PathBuf {
        self.base_dir.join(format!("{key}.meta"))
    }
}

impl BlobStorage for LocalStorage {
    fn put(
        &self,
        key: &str,
        data: bytes::Bytes,
        content_type: &str,
    ) -> BoxFuture<'_, Result<(), StorageError>> {
        let path = self.data_path(key);
        let meta_path = self.meta_path(key);
        let content_type = content_type.to_string();
        let len = data.len();
        let key = key.to_string();

        Box::pin(async move {
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|e| {
                    StorageError::Internal(format!("failed to create directory: {e}"))
                })?;
            }

            tokio::fs::write(&path, &data)
                .await
                .map_err(|e| StorageError::Internal(format!("failed to write blob: {e}")))?;

            tokio::fs::write(&meta_path, content_type.as_bytes())
                .await
                .map_err(|e| StorageError::Internal(format!("failed to write meta: {e}")))?;

            debug!(key = %key, "LocalStorage: wrote blob ({len} bytes)");
            Ok(())
        })
    }

    fn get(&self, key: &str) -> BoxFuture<'_, Result<(bytes::Bytes, BlobMeta), StorageError>> {
        let path = self.data_path(key);
        let meta_path = self.meta_path(key);
        let key = key.to_string();

        Box::pin(async move {
            let data = tokio::fs::read(&path).await.map_err(|e| match e.kind() {
                std::io::ErrorKind::NotFound => StorageError::NotFound(key.clone()),
                _ => StorageError::Internal(format!("failed to read blob: {e}")),
            })?;

            let content_type = tokio::fs::read_to_string(&meta_path)
                .await
                .unwrap_or_else(|_| "application/octet-stream".to_string());

            Ok((bytes::Bytes::from(data), BlobMeta { content_type }))
        })
    }
}

// ---------------------------------------------------------------------------
// S3Storage — S3-compatible backend (Cloudflare R2, AWS S3, MinIO, etc.)
// ---------------------------------------------------------------------------

/// Stores blobs in an S3-compatible object store.
pub struct S3Storage {
    store: Box<dyn ObjectStore>,
}

impl S3Storage {
    pub fn new(
        endpoint: String,
        bucket: String,
        access_key: String,
        secret_key: String,
    ) -> Result<Self, StorageError> {
        let store = AmazonS3Builder::new()
            .with_endpoint(endpoint)
            .with_bucket_name(bucket)
            .with_access_key_id(access_key)
            .with_secret_access_key(secret_key)
            // R2 and most S3-compatible stores use virtual-hosted–style by default,
            // but path-style is more reliable for non-AWS endpoints.
            .with_virtual_hosted_style_request(false)
            .build()
            .map_err(|e| StorageError::Internal(format!("failed to build S3 object store: {e}")))?;

        Ok(Self {
            store: Box::new(store),
        })
    }
}

impl BlobStorage for S3Storage {
    fn put(
        &self,
        key: &str,
        data: bytes::Bytes,
        content_type: &str,
    ) -> BoxFuture<'_, Result<(), StorageError>> {
        let path = object_store::path::Path::from(key);
        let content_type = content_type.to_string();
        let key = key.to_string();

        Box::pin(async move {
            let attrs = object_store::Attributes::from_iter([(
                object_store::Attribute::ContentType,
                object_store::AttributeValue::from(content_type),
            )]);
            let opts = object_store::PutOptions {
                attributes: attrs,
                ..Default::default()
            };

            self.store
                .put_opts(&path, data.into(), opts)
                .await
                .map_err(|e| StorageError::Internal(format!("S3 put failed: {e}")))?;

            debug!(key = %key, "S3Storage: wrote blob");
            Ok(())
        })
    }

    fn get(&self, key: &str) -> BoxFuture<'_, Result<(bytes::Bytes, BlobMeta), StorageError>> {
        let path = object_store::path::Path::from(key);
        let key = key.to_string();

        Box::pin(async move {
            let result = self.store.get(&path).await.map_err(|e| match e {
                object_store::Error::NotFound { .. } => StorageError::NotFound(key),
                other => StorageError::Internal(format!("S3 get failed: {other}")),
            })?;

            let content_type = result
                .attributes
                .get(&object_store::Attribute::ContentType)
                .map(|v| v.to_string())
                .unwrap_or_else(|| "application/octet-stream".to_string());

            let data = result
                .bytes()
                .await
                .map_err(|e| StorageError::Internal(format!("S3 read body failed: {e}")))?;

            Ok((data, BlobMeta { content_type }))
        })
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Build the blob storage backend from environment variables.
///
/// If `WADDLE_S3_ENDPOINT` is set, uses S3-compatible storage (requires
/// `WADDLE_S3_BUCKET`, `WADDLE_S3_ACCESS_KEY_ID`, `WADDLE_S3_SECRET_ACCESS_KEY`).
/// Otherwise falls back to local filesystem storage at `WADDLE_UPLOAD_DIR`
/// (default: `./uploads`).
pub fn build_blob_storage() -> Result<Arc<dyn BlobStorage>, StorageError> {
    if let Ok(endpoint) = std::env::var("WADDLE_S3_ENDPOINT") {
        let bucket = std::env::var("WADDLE_S3_BUCKET").map_err(|_| {
            StorageError::Internal(
                "WADDLE_S3_BUCKET is required when WADDLE_S3_ENDPOINT is set".into(),
            )
        })?;
        let access_key = std::env::var("WADDLE_S3_ACCESS_KEY_ID").map_err(|_| {
            StorageError::Internal(
                "WADDLE_S3_ACCESS_KEY_ID is required when WADDLE_S3_ENDPOINT is set".into(),
            )
        })?;
        let secret_key = std::env::var("WADDLE_S3_SECRET_ACCESS_KEY").map_err(|_| {
            StorageError::Internal(
                "WADDLE_S3_SECRET_ACCESS_KEY is required when WADDLE_S3_ENDPOINT is set".into(),
            )
        })?;

        info!(
            endpoint = %endpoint,
            bucket = %bucket,
            "Using S3-compatible blob storage"
        );
        Ok(Arc::new(S3Storage::new(
            endpoint, bucket, access_key, secret_key,
        )?))
    } else {
        let dir = std::env::var("WADDLE_UPLOAD_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from("./uploads"));

        info!(dir = %dir.display(), "Using local filesystem blob storage");
        Ok(Arc::new(LocalStorage::new(dir)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn local_storage_roundtrip() {
        let dir = std::env::temp_dir().join(format!("waddle-blob-test-{}", uuid::Uuid::new_v4()));
        let storage = LocalStorage::new(dir.clone());

        let key = "test-slot/photo.jpg";
        let data = bytes::Bytes::from_static(b"fake image data");

        // Put
        storage.put(key, data.clone(), "image/jpeg").await.unwrap();

        // Get
        let (got_data, meta) = storage.get(key).await.unwrap();
        assert_eq!(got_data, data);
        assert_eq!(meta.content_type, "image/jpeg");

        // Not found
        let err = storage.get("nonexistent").await;
        assert!(matches!(err, Err(StorageError::NotFound(_))));

        // Cleanup
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[tokio::test]
    async fn dyn_dispatch_works() {
        let dir = std::env::temp_dir().join(format!("waddle-blob-dyn-{}", uuid::Uuid::new_v4()));
        let storage: Arc<dyn BlobStorage> = Arc::new(LocalStorage::new(dir.clone()));

        let key = "dyn-test/file.txt";
        let data = bytes::Bytes::from_static(b"hello");

        storage.put(key, data.clone(), "text/plain").await.unwrap();
        let (got, meta) = storage.get(key).await.unwrap();
        assert_eq!(got, data);
        assert_eq!(meta.content_type, "text/plain");

        let _ = std::fs::remove_dir_all(&dir);
    }
}
