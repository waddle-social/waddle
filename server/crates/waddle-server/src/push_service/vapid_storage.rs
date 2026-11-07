//! Sealed VAPID keypair storage for the first-party XMPP Push Service.
//!
//! Stores ECDSA P-256 private keys for RFC 8292 VAPID JWT signing, sealed
//! at rest with AES-256-GCM and a length-prefixed AAD that binds the
//! sealed blob to its `label` + `kid` so a DB-write attacker swapping
//! blobs across rows or labels fails the GCM tag check.
//!
//! Implements the [`VapidSigner`] trait declared in `waddle-xmpp`,
//! delegating the actual JWT signing to [`InProcessVapidSigner`]. This
//! crate owns the durable lifecycle (env-var bootstrap, fresh
//! generation, DB persistence); the lower crate owns pure crypto.
//!
//! ## Environment policy
//!
//! Bootstrap reads never mutate the process environment: database and telemetry
//! workers may already be reading it. The bootstrap variable remains available
//! to the running process after success and failure. Operators remove
//! `WADDLE_VAPID_PRIVATE_KEY` from the deployment after the first successful
//! boot, once the key is stored. See `docs/operations/vapid-bootstrap.md`.

use crate::db::DatabaseDriver;
use crate::db::DatabaseError;
use crate::db::Rows;
use std::env;
use std::sync::Arc;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::Aes256Gcm;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use hmac::{Hmac, KeyInit as HmacKeyInit, Mac};
use p256::elliptic_curve::rand_core::OsRng;
use p256::elliptic_curve::sec1::ToEncodedPoint;
use rand::RngExt;
use sha2::Sha256;
use uuid::Uuid;
use waddle_xmpp::push::types::{
    Kid, VapidDbReadStage, VapidDbWriteStage, VapidEnvParseError, VapidLoadError,
    VapidUnsealErrorKind,
};
use waddle_xmpp::push::vapid::{InProcessVapidSigner, VapidSigner};
use waddle_xmpp::XmppError;
use zeroize::Zeroizing;

use crate::db::{CatalogFenceLock, Database, IntoParams, Transaction};

pub use super::runtime_attach::VapidRuntimeAttachError;

/// Failure while the trusted schema owner prepares the first durable VAPID
/// signer. This operation is idempotent and does not rotate an existing key.
#[derive(Debug, thiserror::Error)]
pub enum VapidOwnerError {
    #[error("VAPID owner preparation requires PostgreSQL")]
    PostgresRequired,
    #[error("VAPID owner preparation cannot use an attested runtime handle")]
    OwnerHandleRequired,
    #[error("the Push Service owner schema is not complete")]
    PushServiceNotReady,
    #[error("VAPID owner preparation found an invalid completion marker")]
    InvalidMarker,
    #[error("VAPID owner preparation found an unexpected relation shape")]
    InvalidSchema,
    #[error("VAPID owner preparation refuses to create a replacement for retired keys")]
    RotationRequired,
    #[error("VAPID owner preparation found more than one active key")]
    MultipleActiveKeys,
    #[error("VAPID bootstrap environment value is not valid UTF-8")]
    InvalidBootstrapEnvironment,
    #[error("VAPID persisted key failed integrity validation")]
    InvalidStoredKey,
    #[error("VAPID private key sealing failed")]
    SealFailed,
    #[error(transparent)]
    PushService(#[from] super::PushServiceOwnerError),
    #[error(transparent)]
    Schema(#[from] super::runtime_attach::PushServiceAttachError),
    #[error(transparent)]
    Load(#[from] VapidLoadError),
    #[error(transparent)]
    RuntimeAttach(#[from] VapidRuntimeAttachError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

const VAPID_ENV_VAR: &str = "WADDLE_VAPID_PRIVATE_KEY";
const AAD_LABEL: &[u8] = b"waddle:push-service:vapid-key:v1";
const SEALED_PREFIX: &str = "waddle-vapid-v1";
/// Current root-key version. Multi-version rotation lands in a follow-up;
/// for v1, all rows are sealed at version 1 and read at version 1.
const CURRENT_ROOT_KEY_VERSION: u32 = 1;

/// Durable VAPID keypair storage for the Push Service component.
///
/// Construct via [`Self::load_or_provision`]. PostgreSQL selects or creates
/// the first key under one fenced transaction; SQLite retains its legacy
/// environment, latest-key, then generation order.
pub struct VapidStorage {
    db: Database,
    cipher: VapidKeyCipher,
}

impl VapidStorage {
    /// Prepare the VAPID table and its first durable signer through the exact
    /// Push Service schema-owner handle. The owner marker is committed last in
    /// the same transaction, so runtime attachment can distinguish complete
    /// preparation from an absent or interrupted owner phase.
    pub async fn prepare_owner(
        db: &Database,
        identity: &crate::db::lineage::PgIdentity,
        root_key: &[u8],
    ) -> Result<(), VapidOwnerError> {
        super::owner::prepare_vapid_owner(db, identity, root_key).await
    }

    /// Load the owner-prepared signer through a bounded, read-only transaction
    /// on the physically attested main pool. This path never runs DDL, reads
    /// the bootstrap environment variable, provisions a replacement key, or
    /// opens another database connection.
    pub async fn load_attested(
        db: Database,
        root_key: &[u8],
    ) -> Result<Arc<dyn VapidSigner>, VapidRuntimeAttachError> {
        let record = super::runtime_attach::load_vapid_key_record(&db).await?;
        if record.root_key_version != i64::from(CURRENT_ROOT_KEY_VERSION) {
            return Err(VapidRuntimeAttachError::UnknownRootKeyVersion);
        }
        let kid_uuid = record
            .kid
            .parse::<Uuid>()
            .map_err(|_| VapidRuntimeAttachError::InvalidKid)?;
        let kid = Kid(kid_uuid);
        let cipher = VapidKeyCipher::new(root_key);
        let scalar = cipher
            .open(&record.sealed_private_key, AAD_LABEL, kid.as_bytes())
            .map_err(VapidRuntimeAttachError::Unseal)?;
        let secret_key = p256::SecretKey::from_slice(scalar.as_slice())
            .map_err(|_| VapidRuntimeAttachError::InvalidPrivateKey)?;
        if secret_key.public_key().to_encoded_point(false).as_bytes()
            != record.public_key.as_slice()
        {
            return Err(VapidRuntimeAttachError::PublicKeyMismatch);
        }
        let signer = InProcessVapidSigner::try_new(kid, secret_key)
            .map_err(|_| VapidRuntimeAttachError::SignerInit)?;
        Ok(Arc::new(signer))
    }

    /// Create a placeholder for a permanently unready node after lineage
    /// attestation fails. This does not access storage or the environment.
    pub fn generate_ephemeral_signer() -> Result<Arc<dyn VapidSigner>, VapidLoadError> {
        let kid = Kid::new();
        let secret_key = p256::SecretKey::random(&mut OsRng);
        Ok(Arc::new(
            InProcessVapidSigner::try_new(kid, secret_key)
                .map_err(|source| VapidLoadError::SignerInit { source })?,
        ))
    }

    pub async fn load_or_provision(
        db: Database,
        root_key: &[u8],
    ) -> Result<Arc<dyn VapidSigner>, VapidLoadError> {
        if db.driver() == DatabaseDriver::Postgres {
            if db.uses_attested_postgres_pool() {
                return Err(VapidLoadError::DbWrite {
                    stage: VapidDbWriteStage::Initialize,
                    source: Box::new(XmppError::internal(
                        "attested VAPID runtime handles must use load_attested",
                    )),
                });
            }
            return Self::load_or_provision_postgres(&db, root_key).await;
        }
        let storage = Self::try_new(db.clone(), root_key)
            .await
            .map_err(|source| VapidLoadError::DbWrite {
                stage: VapidDbWriteStage::Initialize,
                source: Box::new(source),
            })?;

        // 1. Env-var bootstrap path.
        //
        // Initialize the signer before persisting, so any error fails boot
        // without persisting a key that cannot sign. Never clear the process
        // environment: other runtime/database threads may be reading it.
        if let Ok(raw) = env::var(VAPID_ENV_VAR) {
            let raw = Zeroizing::new(raw);
            let secret_key = parse_env_scalar(raw.as_str())?;
            let kid = Kid::new();
            let scalar_bytes = Zeroizing::new(secret_key.to_bytes());
            let public_bytes = secret_key
                .public_key()
                .to_encoded_point(false)
                .as_bytes()
                .to_vec();
            // (a) signer-init (consumes secret_key — fallible).
            let signer = InProcessVapidSigner::try_new(kid, secret_key)
                .map_err(|source| VapidLoadError::SignerInit { source })?;
            // (b) persist (fallible).
            storage
                .persist_keypair(&kid, &scalar_bytes[..], &public_bytes)
                .await?;
            return Ok(Arc::new(signer));
        }

        // 2. Read latest non-retired row.
        if let Some((kid, secret_key)) = storage.load_latest().await? {
            return Ok(Arc::new(
                InProcessVapidSigner::try_new(kid, secret_key)
                    .map_err(|source| VapidLoadError::SignerInit { source })?,
            ));
        }

        // 3. Generate fresh.
        let kid = Kid::new();
        let secret_key = p256::SecretKey::random(&mut OsRng);
        let scalar_bytes = Zeroizing::new(secret_key.to_bytes());
        let public_bytes = secret_key
            .public_key()
            .to_encoded_point(false)
            .as_bytes()
            .to_vec();
        let signer = InProcessVapidSigner::try_new(kid, secret_key)
            .map_err(|source| VapidLoadError::SignerInit { source })?;
        storage
            .persist_keypair(&kid, &scalar_bytes[..], &public_bytes)
            .await?;
        Ok(Arc::new(signer))
    }

    async fn load_or_provision_postgres(
        db: &Database,
        root_key: &[u8],
    ) -> Result<Arc<dyn VapidSigner>, VapidLoadError> {
        let mut tx = db
            .begin_catalog_fenced(&[CatalogFenceLock::Exclusive(
                super::owner::PUSH_SCHEMA_ADVISORY_LOCK_KEY,
            )])
            .await
            .map_err(pg_provision_error)?;
        tx.execute("SET LOCAL lock_timeout = '3s'", ())
            .await
            .map_err(pg_provision_error)?;
        tx.execute("SET LOCAL statement_timeout = '30000ms'", ())
            .await
            .map_err(pg_provision_error)?;

        let mut rows = tx
            .query(
                "SELECT pg_catalog.current_schema()::text, NOT pg_catalog.pg_is_in_recovery()",
                (),
            )
            .await
            .map_err(pg_provision_error)?;
        let row = rows
            .next()
            .await
            .map_err(pg_provision_error)?
            .ok_or(VapidLoadError::InvalidStorageState)?;
        let schema = row
            .get::<Option<String>>(0)
            .map_err(pg_provision_error)?
            .ok_or(VapidLoadError::InvalidStorageState)?;
        if !row.get::<bool>(1).map_err(pg_provision_error)? {
            return Err(VapidLoadError::InvalidStorageState);
        }
        drop(rows);
        if schema.is_empty() {
            return Err(VapidLoadError::InvalidStorageState);
        }
        super::owner::verify_no_temp_push_relations_tx(&mut tx)
            .await
            .map_err(|_| VapidLoadError::InvalidStorageState)?;
        super::owner::lock_existing_push_relations_tx(&mut tx, &schema)
            .await
            .map_err(pg_provision_error)?;

        let table_exists = super::runtime_attach::vapid_relation_exists_tx(
            &mut tx,
            &schema,
            super::runtime_attach::VAPID_TABLE_NAME,
        )
        .await
        .map_err(pg_provision_error)?;
        let vapid_marker_exists = super::runtime_attach::vapid_relation_exists_tx(
            &mut tx,
            &schema,
            super::runtime_attach::VAPID_MARKER_TABLE_NAME,
        )
        .await
        .map_err(pg_provision_error)?;
        let push_marker_exists = super::runtime_attach::vapid_relation_exists_tx(
            &mut tx,
            &schema,
            "push_service_schema_marker",
        )
        .await
        .map_err(pg_provision_error)?;
        if vapid_marker_exists && (!table_exists || !push_marker_exists) {
            return Err(VapidLoadError::InvalidStorageState);
        }

        let identity = if push_marker_exists || vapid_marker_exists {
            Some(live_identity_for_markers_tx(&mut tx, &schema).await?)
        } else {
            None
        };
        if let Some(identity) = &identity {
            if push_marker_exists {
                // An incomplete marker is expected after a raw initializer,
                // but malformed or foreign marker data is never accepted.
                super::runtime_attach::verify_marker_shape_and_data_tx(&mut tx, identity)
                    .await
                    .map_err(|_| VapidLoadError::InvalidStorageState)?;
            }
        }
        let marked_kid = if vapid_marker_exists {
            super::runtime_attach::verify_vapid_marker_shape_tx(&mut tx, &schema)
                .await
                .map_err(|_| VapidLoadError::InvalidStorageState)?;
            let identity = identity
                .as_ref()
                .ok_or(VapidLoadError::InvalidStorageState)?;
            Some(
                super::runtime_attach::verify_vapid_marker_data_tx(&mut tx, identity)
                    .await
                    .map_err(|_| VapidLoadError::InvalidStorageState)?,
            )
        } else {
            None
        };
        let bootstrap = match env::var(VAPID_ENV_VAR) {
            Ok(raw) => Some(parse_env_scalar(Zeroizing::new(raw).as_str())?),
            Err(env::VarError::NotPresent) => None,
            Err(env::VarError::NotUnicode(_)) => {
                return Err(VapidLoadError::InvalidBootstrapEnvironment);
            }
        };

        if !table_exists {
            // DDL and any first insert invalidate a completed Push marker in
            // this same transaction. Only the owner phase may restore it.
            super::owner::invalidate_completion_marker_tx(&mut tx, &schema)
                .await
                .map_err(pg_provision_error)?;
            let quoted_schema = crate::db::physical_attestation::quote_identifier(&schema);
            tx.execute(
                &format!(
                    "CREATE TABLE {quoted_schema}.push_service_vapid_keys (\
                       kid TEXT NOT NULL PRIMARY KEY, sealed_private_key TEXT NOT NULL, \
                       public_key BYTEA NOT NULL, root_key_version BIGINT NOT NULL, \
                       created_at_ms BIGINT NOT NULL, retired_at_ms BIGINT)"
                ),
                (),
            )
            .await
            .map_err(pg_provision_error)?;
            tx.execute(
                &format!(
                    "CREATE INDEX idx_push_service_vapid_keys_active \
                     ON {quoted_schema}.push_service_vapid_keys (retired_at_ms, created_at_ms)"
                ),
                (),
            )
            .await
            .map_err(pg_provision_error)?;
        }
        if vapid_marker_exists {
            super::runtime_attach::verify_vapid_table_shape_tx(&mut tx, &schema)
                .await
                .map_err(|_| VapidLoadError::InvalidStorageState)?;
        } else if super::runtime_attach::verify_raw_vapid_table_shape_tx(&mut tx, &schema)
            .await
            .map_err(|_| VapidLoadError::InvalidStorageState)?
        {
            // Old raw initialization created this index if absent. Only
            // repair it after validating the remaining table shape; never
            // repair an owner-marked relation here.
            super::owner::invalidate_completion_marker_tx(&mut tx, &schema)
                .await
                .map_err(pg_provision_error)?;
            let quoted_schema = crate::db::physical_attestation::quote_identifier(&schema);
            tx.execute(
                &format!(
                    "CREATE INDEX idx_push_service_vapid_keys_active \
                     ON {quoted_schema}.push_service_vapid_keys (retired_at_ms, created_at_ms)"
                ),
                (),
            )
            .await
            .map_err(pg_provision_error)?;
            if super::runtime_attach::verify_raw_vapid_table_shape_tx(&mut tx, &schema)
                .await
                .map_err(|_| VapidLoadError::InvalidStorageState)?
            {
                return Err(VapidLoadError::InvalidStorageState);
            }
        }

        let active_count = super::runtime_attach::count_active_vapid_keys_tx(&mut tx, &schema)
            .await
            .map_err(pg_provision_error)?;
        if active_count > 1 {
            return Err(VapidLoadError::MultipleActiveKeys);
        }
        let record = super::runtime_attach::select_vapid_active_key_tx(&mut tx, &schema)
            .await
            .map_err(pg_provision_error)?;
        if let Some(marked_kid) = marked_kid {
            if record.as_ref().map(|record| record.kid.as_str()) != Some(marked_kid.as_str()) {
                return Err(VapidLoadError::InvalidStorageState);
            }
        }
        let cipher = VapidKeyCipher::new(root_key);
        let signer = if let Some(record) = record {
            let (kid, secret_key) = decode_persisted_key(&cipher, &record)
                .map_err(|_| VapidLoadError::InvalidStoredKey)?;
            if bootstrap.as_ref().is_some_and(|key| {
                key.public_key().to_encoded_point(false).as_bytes() != record.public_key.as_slice()
            }) {
                return Err(VapidLoadError::BootstrapKeyMismatch);
            }
            InProcessVapidSigner::try_new(kid, secret_key)
                .map_err(|source| VapidLoadError::SignerInit { source })?
        } else {
            if super::runtime_attach::count_vapid_keys_tx(&mut tx, &schema)
                .await
                .map_err(pg_provision_error)?
                != 0
            {
                return Err(VapidLoadError::RotationRequired);
            }
            let secret_key = bootstrap.unwrap_or_else(|| p256::SecretKey::random(&mut OsRng));
            let kid = Kid::new();
            let scalar_bytes = Zeroizing::new(secret_key.to_bytes());
            let public_bytes = secret_key
                .public_key()
                .to_encoded_point(false)
                .as_bytes()
                .to_vec();
            let signer = InProcessVapidSigner::try_new(kid, secret_key)
                .map_err(|source| VapidLoadError::SignerInit { source })?;
            let sealed = cipher
                .seal(&scalar_bytes[..], AAD_LABEL, kid.as_bytes())
                .map_err(|source| VapidLoadError::DbWrite {
                    stage: VapidDbWriteStage::SealPrivateKey,
                    source: Box::new(source),
                })?;
            super::owner::invalidate_completion_marker_tx(&mut tx, &schema)
                .await
                .map_err(pg_provision_error)?;
            let quoted_schema = crate::db::physical_attestation::quote_identifier(&schema);
            tx.execute(
                &format!(
                    "INSERT INTO {quoted_schema}.push_service_vapid_keys \
                     (kid, sealed_private_key, public_key, root_key_version, created_at_ms) \
                     VALUES (?, ?, ?, ?, ?)"
                ),
                crate::db_params![
                    kid.to_string(),
                    sealed,
                    public_bytes,
                    i64::from(CURRENT_ROOT_KEY_VERSION),
                    chrono::Utc::now().timestamp_millis(),
                ],
            )
            .await
            .map_err(pg_provision_error)?;
            signer
        };
        // The catalog-fenced transaction discards its checkout when COMMIT is
        // cancelled or unacknowledged. A later call must reacquire both fences
        // and read durable state; it cannot infer rollback from socket closure.
        tx.commit()
            .await
            .map_err(|_| VapidLoadError::CommitIndeterminate)?;
        Ok(Arc::new(signer))
    }

    async fn try_new(db: Database, root_key: &[u8]) -> Result<Self, XmppError> {
        let storage = Self {
            db,
            cipher: VapidKeyCipher::new(root_key),
        };
        storage.initialize().await?;
        Ok(storage)
    }

    async fn initialize(&self) -> Result<(), XmppError> {
        if self.db.uses_attested_postgres_pool() {
            return Err(XmppError::internal(
                "attested VAPID runtime handles must use load_attested",
            ));
        }

        let driver = self.db.driver();
        let i64_type = crate::db::i64_sql_type(self.db.driver());
        // `BLOB` is the SQLite byte type; Postgres requires `BYTEA`. Use the
        // configured driver to emit the dialect-correct column type.
        let blob_type = match self.db.driver() {
            DatabaseDriver::Sqlite => "BLOB",
            DatabaseDriver::Postgres => "BYTEA",
        };
        // `kid` is a UUIDv4 and already `NOT NULL UNIQUE`; using it as the
        // primary key avoids the SQLite-vs-Postgres autoincrement-integer
        // mismatch that an `id INTEGER PRIMARY KEY` column would create.
        let sql = format!(
            r#"
            CREATE TABLE IF NOT EXISTS push_service_vapid_keys (
                kid TEXT NOT NULL PRIMARY KEY,
                sealed_private_key TEXT NOT NULL,
                public_key {blob_type} NOT NULL,
                root_key_version {i64_type} NOT NULL,
                created_at_ms {i64_type} NOT NULL,
                retired_at_ms {i64_type}
            )
            "#
        );
        let index_sql = "CREATE INDEX IF NOT EXISTS idx_push_service_vapid_keys_active \
            ON push_service_vapid_keys (retired_at_ms, created_at_ms)";
        match driver {
            DatabaseDriver::Sqlite => {
                self.run_execute(&sql, ()).await?;
                self.run_execute(index_sql, ()).await?;
            }
            DatabaseDriver::Postgres => {
                let mut tx = self
                    .db
                    .begin_catalog_fenced(&[CatalogFenceLock::Exclusive(
                        super::owner::PUSH_SCHEMA_ADVISORY_LOCK_KEY,
                    )])
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                tx.execute("SET LOCAL lock_timeout = '3s'", ())
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                tx.execute("SET LOCAL statement_timeout = '30000ms'", ())
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                let mut rows = tx
                    .query("SELECT pg_catalog.current_schema()::text", ())
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                let schema = rows
                    .next()
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?
                    .and_then(|row| row.get::<Option<String>>(0).ok().flatten())
                    .ok_or_else(|| XmppError::internal("PostgreSQL VAPID schema is unavailable"))?;
                drop(rows);
                super::owner::verify_no_temp_push_relations_tx(&mut tx)
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                super::owner::lock_existing_push_relations_tx(&mut tx, &schema)
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                super::owner::invalidate_completion_marker_tx(&mut tx, &schema)
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                tx.execute(&sql, ())
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                tx.execute(index_sql, ())
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                tx.commit()
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
            }
        }
        Ok(())
    }

    async fn run_execute(&self, sql: &str, params: impl IntoParams) -> Result<u64, XmppError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|e| XmppError::internal(e.to_string()))?;
        conn.execute(sql, params)
            .await
            .map_err(|e| XmppError::internal(e.to_string()))
    }

    async fn run_query(&self, sql: &str, params: impl IntoParams) -> Result<Rows, XmppError> {
        let conn = self
            .db
            .guard()
            .await
            .map_err(|e| XmppError::internal(e.to_string()))?;
        conn.query(sql, params)
            .await
            .map_err(|e| XmppError::internal(e.to_string()))
    }

    /// Persist a sealed VAPID keypair from pre-derived byte material.
    ///
    /// Taking bytes rather than the `SecretKey` itself lets callers
    /// construct the in-process signer (which consumes the `SecretKey`)
    /// before the persist step, so signer initialization failure cannot leave
    /// behind a newly persisted unusable key.
    ///
    /// `scalar_bytes` MUST be 32 bytes (a raw P-256 secret scalar) and
    /// is wrapped in `Zeroizing` by the caller. `public_bytes` is the
    /// 65-byte uncompressed SEC1 point (public, no zeroize needed).
    async fn persist_keypair(
        &self,
        kid: &Kid,
        scalar_bytes: &[u8],
        public_bytes: &[u8],
    ) -> Result<(), VapidLoadError> {
        let now_ms = chrono::Utc::now().timestamp_millis();
        let sealed = self
            .cipher
            .seal(scalar_bytes, AAD_LABEL, kid.as_bytes())
            .map_err(|source| VapidLoadError::DbWrite {
                stage: VapidDbWriteStage::SealPrivateKey,
                source: Box::new(source),
            })?;

        let sql = "INSERT INTO push_service_vapid_keys \
            (kid, sealed_private_key, public_key, root_key_version, created_at_ms) \
            VALUES (?, ?, ?, ?, ?)";
        self.run_execute(
            sql,
            crate::db_params![
                kid.to_string(),
                sealed,
                public_bytes.to_vec(),
                CURRENT_ROOT_KEY_VERSION as i64,
                now_ms,
            ],
        )
        .await
        .map_err(|source| VapidLoadError::DbWrite {
            stage: VapidDbWriteStage::InsertRow,
            source: Box::new(source),
        })?;
        Ok(())
    }

    async fn load_latest(&self) -> Result<Option<(Kid, p256::SecretKey)>, VapidLoadError> {
        let sql = "SELECT kid, sealed_private_key, root_key_version \
            FROM push_service_vapid_keys \
            WHERE retired_at_ms IS NULL \
            ORDER BY created_at_ms DESC LIMIT 1";
        let mut rows = self
            .run_query(sql, ())
            .await
            .map_err(|source| VapidLoadError::DbRead {
                stage: VapidDbReadStage::Query,
                source: Box::new(source),
            })?;
        let Some(row) = rows.next().await.map_err(|source| VapidLoadError::DbRead {
            stage: VapidDbReadStage::NextRow,
            source: Box::new(source),
        })?
        else {
            return Ok(None);
        };
        let kid_str: String = row.get(0).map_err(|source| VapidLoadError::DbRead {
            stage: VapidDbReadStage::DecodeKid,
            source: Box::new(source),
        })?;
        let sealed: String = row.get(1).map_err(|source| VapidLoadError::DbRead {
            stage: VapidDbReadStage::DecodeSealedPrivateKey,
            source: Box::new(source),
        })?;
        let version: i64 = row.get(2).map_err(|source| VapidLoadError::DbRead {
            stage: VapidDbReadStage::DecodeRootKeyVersion,
            source: Box::new(source),
        })?;
        // Compare as `i64` directly to avoid the `as u32` wraparound foot-gun:
        // a negative value (or any v where `(v % 2^32) == 1`) would silently
        // pass an `i64 as u32 == 1` check.
        if version != i64::from(CURRENT_ROOT_KEY_VERSION) {
            return Err(VapidLoadError::UnknownRootKeyVersion {
                found: u32::try_from(version).unwrap_or(u32::MAX),
                max_installed: CURRENT_ROOT_KEY_VERSION,
            });
        }
        let kid_uuid: Uuid =
            kid_str
                .parse()
                .map_err(|source: uuid::Error| VapidLoadError::DbRead {
                    stage: VapidDbReadStage::ParseKidUuid,
                    source: Box::new(source),
                })?;
        let kid = Kid(kid_uuid);
        let scalar = self
            .cipher
            .open(&sealed, AAD_LABEL, kid.as_bytes())
            .map_err(|kind| VapidLoadError::Unseal { kind })?;
        let secret_key = p256::SecretKey::from_slice(scalar.as_slice()).map_err(|source| {
            VapidLoadError::DbRead {
                stage: VapidDbReadStage::ParseSecretKeyScalar,
                source: Box::new(source),
            }
        })?;
        // `scalar` is `Zeroizing<Vec<u8>>` and zeroes here.
        Ok(Some((kid, secret_key)))
    }
}

fn pg_provision_error(source: impl std::error::Error + Send + Sync + 'static) -> VapidLoadError {
    VapidLoadError::DbWrite {
        stage: VapidDbWriteStage::FencedProvision,
        source: Box::new(source),
    }
}

/// Read marker identity from the same fenced backend, never a pool checkout
/// that could name a different database or schema.
async fn live_identity_for_markers_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<crate::db::lineage::PgIdentity, VapidLoadError> {
    use crate::db::lineage::{
        PgDatabaseIdentity, PgDatabaseName, PgIdentity, PgSchemaIdentity, PgSchemaName,
    };

    let mut rows = tx
        .query(
            "SELECT (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
               database.oid::bigint, database.datname::text, namespace.oid::bigint, \
               namespace.nspname::text \
             FROM pg_catalog.pg_namespace namespace \
             JOIN pg_catalog.pg_database database \
               ON database.datname = pg_catalog.current_database() \
             WHERE namespace.nspname = ? AND namespace.nspname = pg_catalog.current_schema()",
            crate::db_params![schema],
        )
        .await
        .map_err(pg_provision_error)?;
    let row = rows
        .next()
        .await
        .map_err(pg_provision_error)?
        .ok_or(VapidLoadError::InvalidStorageState)?;
    let system_identifier = row
        .get::<String>(0)
        .map_err(pg_provision_error)?
        .parse()
        .map_err(|_| VapidLoadError::InvalidStorageState)?;
    let database_oid = u32::try_from(row.get::<i64>(1).map_err(pg_provision_error)?)
        .map_err(|_| VapidLoadError::InvalidStorageState)?;
    let database_name = row.get::<String>(2).map_err(pg_provision_error)?;
    let schema_oid = u32::try_from(row.get::<i64>(3).map_err(pg_provision_error)?)
        .map_err(|_| VapidLoadError::InvalidStorageState)?;
    let schema_name = row.get::<String>(4).map_err(pg_provision_error)?;
    Ok(PgIdentity {
        system_identifier,
        database: PgDatabaseIdentity {
            oid: database_oid,
            name: PgDatabaseName(database_name),
        },
        schema: PgSchemaIdentity {
            oid: schema_oid,
            name: PgSchemaName(schema_name),
        },
    })
}

/// Owner-phase implementation. The caller holds the existing Push schema
/// advisory lock and has proved that the Push Service owner marker is ready.
pub(super) async fn prepare_vapid_owner_tx(
    tx: &mut Transaction<'_>,
    identity: &crate::db::lineage::PgIdentity,
    root_key: &[u8],
) -> Result<(), VapidOwnerError> {
    let schema = &identity.schema.name.0;
    let table_exists = super::runtime_attach::vapid_relation_exists_tx(
        tx,
        schema,
        super::runtime_attach::VAPID_TABLE_NAME,
    )
    .await?;
    let marker_exists = super::runtime_attach::vapid_relation_exists_tx(
        tx,
        schema,
        super::runtime_attach::VAPID_MARKER_TABLE_NAME,
    )
    .await?;
    if marker_exists && !table_exists {
        return Err(VapidOwnerError::InvalidSchema);
    }

    let quoted_schema = crate::db::physical_attestation::quote_identifier(schema);
    if !table_exists {
        let table = crate::db::physical_attestation::quote_identifier(
            super::runtime_attach::VAPID_TABLE_NAME,
        );
        let index =
            crate::db::physical_attestation::quote_identifier("idx_push_service_vapid_keys_active");
        tx.execute(
            &format!(
                "CREATE TABLE {quoted_schema}.{table} (\
                   kid TEXT NOT NULL PRIMARY KEY, \
                   sealed_private_key TEXT NOT NULL, \
                   public_key BYTEA NOT NULL, \
                   root_key_version BIGINT NOT NULL, \
                   created_at_ms BIGINT NOT NULL, \
                   retired_at_ms BIGINT)"
            ),
            (),
        )
        .await?;
        tx.execute(
            &format!(
                "CREATE INDEX {index} ON {quoted_schema}.{table} \
                 (retired_at_ms, created_at_ms)"
            ),
            (),
        )
        .await?;
    }
    super::runtime_attach::verify_vapid_table_shape_tx(tx, schema)
        .await
        .map_err(VapidOwnerError::Schema)?;

    let cipher = VapidKeyCipher::new(root_key);
    if marker_exists {
        super::runtime_attach::verify_vapid_marker_shape_tx(tx, schema)
            .await
            .map_err(VapidOwnerError::Schema)?;
        let marked_kid = super::runtime_attach::verify_vapid_marker_data_tx(tx, identity).await?;
        let active_count = super::runtime_attach::count_active_vapid_keys_tx(tx, schema).await?;
        if active_count == 0 {
            return Err(VapidOwnerError::InvalidMarker);
        }
        if active_count > 1 {
            return Err(VapidOwnerError::MultipleActiveKeys);
        }
        let record = super::runtime_attach::select_vapid_active_key_tx(tx, schema)
            .await?
            .ok_or(VapidOwnerError::InvalidMarker)?;
        if record.kid != marked_kid {
            return Err(VapidOwnerError::InvalidMarker);
        }
        validate_persisted_key(&cipher, &record).map_err(|_| VapidOwnerError::InvalidStoredKey)?;
        return Ok(());
    }

    let active_count = super::runtime_attach::count_active_vapid_keys_tx(tx, schema).await?;
    if active_count > 1 {
        return Err(VapidOwnerError::MultipleActiveKeys);
    }
    let active = super::runtime_attach::select_vapid_active_key_tx(tx, schema).await?;
    let initial_kid = if let Some(record) = active {
        validate_persisted_key(&cipher, &record).map_err(|_| VapidOwnerError::InvalidStoredKey)?;
        record.kid
    } else {
        if super::runtime_attach::count_vapid_keys_tx(tx, schema).await? != 0 {
            return Err(VapidOwnerError::RotationRequired);
        }
        provision_first_key_tx(tx, schema, &cipher).await?
    };

    let marker = crate::db::physical_attestation::quote_identifier(
        super::runtime_attach::VAPID_MARKER_TABLE_NAME,
    );
    tx.execute(
        &format!(
            "CREATE TABLE {quoted_schema}.{marker} (\
               marker_id INTEGER NOT NULL PRIMARY KEY CHECK (marker_id = 1), \
               version BIGINT NOT NULL CHECK (version = 1), \
               system_identifier TEXT NOT NULL, \
               database_oid BIGINT NOT NULL, \
               database_name TEXT NOT NULL, \
               schema_oid BIGINT NOT NULL, \
               schema_name TEXT NOT NULL, \
               initial_kid TEXT NOT NULL)"
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "INSERT INTO {quoted_schema}.{marker} \
             (marker_id, version, system_identifier, database_oid, database_name, \
              schema_oid, schema_name, initial_kid) \
             VALUES (1, 1, ?, ?, ?, ?, ?, ?)"
        ),
        crate::db_params![
            identity.system_identifier.to_string(),
            i64::from(identity.database.oid),
            identity.database.name.0.clone(),
            i64::from(identity.schema.oid),
            identity.schema.name.0.clone(),
            initial_kid,
        ],
    )
    .await?;
    super::runtime_attach::verify_vapid_marker_shape_tx(tx, schema)
        .await
        .map_err(VapidOwnerError::Schema)?;
    Ok(())
}

async fn provision_first_key_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
    cipher: &VapidKeyCipher,
) -> Result<String, VapidOwnerError> {
    let secret_key = match env::var(VAPID_ENV_VAR) {
        Ok(raw) => {
            let raw = Zeroizing::new(raw);
            parse_env_scalar(raw.as_str())?
        }
        Err(env::VarError::NotPresent) => p256::SecretKey::random(&mut OsRng),
        Err(env::VarError::NotUnicode(_)) => {
            return Err(VapidOwnerError::InvalidBootstrapEnvironment);
        }
    };
    let kid = Kid::new();
    let scalar_bytes = Zeroizing::new(secret_key.to_bytes());
    let public_bytes = secret_key
        .public_key()
        .to_encoded_point(false)
        .as_bytes()
        .to_vec();
    let _signer = InProcessVapidSigner::try_new(kid, secret_key)
        .map_err(|source| VapidOwnerError::Load(VapidLoadError::SignerInit { source }))?;
    let sealed = cipher
        .seal(&scalar_bytes[..], AAD_LABEL, kid.as_bytes())
        .map_err(|_| VapidOwnerError::SealFailed)?;
    let quoted_schema = crate::db::physical_attestation::quote_identifier(schema);
    let table =
        crate::db::physical_attestation::quote_identifier(super::runtime_attach::VAPID_TABLE_NAME);
    tx.execute(
        &format!(
            "INSERT INTO {quoted_schema}.{table} \
             (kid, sealed_private_key, public_key, root_key_version, created_at_ms) \
             VALUES (?, ?, ?, ?, ?)"
        ),
        crate::db_params![
            kid.to_string(),
            sealed,
            public_bytes,
            i64::from(CURRENT_ROOT_KEY_VERSION),
            chrono::Utc::now().timestamp_millis(),
        ],
    )
    .await?;
    Ok(kid.to_string())
}

/// Check a selected owner-prepared key without constructing or returning a
/// signer. The VAPID custody diagnostic invokes this while its shared
/// read-only transaction still holds the VAPID relation locks.
pub(super) fn verify_persisted_key_for_attachment(
    root_key: &[u8],
    record: &super::runtime_attach::VapidKeyRecord,
) -> Result<(), VapidRuntimeAttachError> {
    validate_persisted_key(&VapidKeyCipher::new(root_key), record)
}

fn validate_persisted_key(
    cipher: &VapidKeyCipher,
    record: &super::runtime_attach::VapidKeyRecord,
) -> Result<(), VapidRuntimeAttachError> {
    decode_persisted_key(cipher, record).map(|_| ())
}

fn decode_persisted_key(
    cipher: &VapidKeyCipher,
    record: &super::runtime_attach::VapidKeyRecord,
) -> Result<(Kid, p256::SecretKey), VapidRuntimeAttachError> {
    if record.root_key_version != i64::from(CURRENT_ROOT_KEY_VERSION) {
        return Err(VapidRuntimeAttachError::UnknownRootKeyVersion);
    }
    let kid_uuid = record
        .kid
        .parse::<Uuid>()
        .map_err(|_| VapidRuntimeAttachError::InvalidKid)?;
    let kid = Kid(kid_uuid);
    let scalar = cipher
        .open(&record.sealed_private_key, AAD_LABEL, kid.as_bytes())
        .map_err(VapidRuntimeAttachError::Unseal)?;
    let secret_key = p256::SecretKey::from_slice(scalar.as_slice())
        .map_err(|_| VapidRuntimeAttachError::InvalidPrivateKey)?;
    if secret_key.public_key().to_encoded_point(false).as_bytes() != record.public_key.as_slice() {
        return Err(VapidRuntimeAttachError::PublicKeyMismatch);
    }
    Ok((kid, secret_key))
}

/// AES-256-GCM sealing for VAPID private keys.
///
/// AAD is **length-prefixed** to prevent canonicalization collisions:
/// `u16_be(len(label)) || label || u16_be(len(kid_bytes)) || kid_bytes`.
struct VapidKeyCipher {
    /// Initialized once at storage construction. AES-256-GCM key was derived
    /// from the root key via HMAC-SHA256 with a domain-separating label.
    cipher: Aes256Gcm,
}

#[derive(Debug, thiserror::Error)]
enum VapidSealError {
    #[error("AES-256-GCM seal failed")]
    EncryptFailed,
}

impl VapidKeyCipher {
    fn new(root_key: &[u8]) -> Self {
        let mut mac = <Hmac<Sha256> as HmacKeyInit>::new_from_slice(root_key)
            .expect("HMAC-SHA256 accepts arbitrary-length keys");
        mac.update(b"waddle:push-service:vapid-key:enc:v1");
        // `hmac 0.13` emits the newer `hybrid_array::Array<u8, U32>`, while
        // `aes-gcm 0.10` still consumes the legacy `GenericArray<u8, U32>`.
        // The two are layout-compatible but not type-compatible, so we
        // route through `new_from_slice` (which takes `&[u8]`); the
        // length check is statically guaranteed by the 32-byte MAC output.
        let derived = mac.finalize().into_bytes();
        let cipher = Aes256Gcm::new_from_slice(&derived)
            .expect("HMAC-SHA256 output is 32 bytes — exactly the AES-256 key size");
        Self { cipher }
    }

    fn seal(
        &self,
        plaintext: &[u8],
        label: &[u8],
        kid_bytes: &[u8],
    ) -> Result<String, VapidSealError> {
        // Length-prefix encoding uses u16, so each field is capped at 65535
        // bytes. Practical labels (~32 B) and kids (16 B UUID) are far below
        // the bound; the assertion catches future regressions.
        assert!(label.len() <= u16::MAX as usize, "AAD label too long");
        assert!(kid_bytes.len() <= u16::MAX as usize, "AAD kid too long");
        let aad = build_aad(label, kid_bytes);
        let mut nonce = [0u8; 12];
        rand::rng().fill(&mut nonce[..]);
        let ciphertext = self
            .cipher
            .encrypt(
                (&nonce).into(),
                Payload {
                    msg: plaintext,
                    aad: &aad,
                },
            )
            .map_err(|_| VapidSealError::EncryptFailed)?;
        Ok(format!(
            "{SEALED_PREFIX}:{}:{}",
            URL_SAFE_NO_PAD.encode(nonce),
            URL_SAFE_NO_PAD.encode(ciphertext)
        ))
    }

    fn open(
        &self,
        stored: &str,
        label: &[u8],
        kid_bytes: &[u8],
    ) -> Result<Zeroizing<Vec<u8>>, VapidUnsealErrorKind> {
        let mut parts = stored.split(':');
        let prefix = parts.next().ok_or(VapidUnsealErrorKind::MissingPrefix)?;
        if prefix != SEALED_PREFIX {
            return Err(VapidUnsealErrorKind::UnexpectedPrefix);
        }
        let nonce_b64 = parts.next().ok_or(VapidUnsealErrorKind::MissingNonce)?;
        let ct_b64 = parts
            .next()
            .ok_or(VapidUnsealErrorKind::MissingCiphertext)?;
        if parts.next().is_some() {
            return Err(VapidUnsealErrorKind::UnexpectedTrailingField);
        }
        let nonce = URL_SAFE_NO_PAD
            .decode(nonce_b64)
            .map_err(|_| VapidUnsealErrorKind::NonceBase64urlDecode)?;
        let nonce: [u8; 12] = nonce
            .try_into()
            .map_err(|_: Vec<u8>| VapidUnsealErrorKind::NonceWrongLength)?;
        let ciphertext = URL_SAFE_NO_PAD
            .decode(ct_b64)
            .map_err(|_| VapidUnsealErrorKind::CiphertextBase64urlDecode)?;
        let aad = build_aad(label, kid_bytes);
        let plaintext = self
            .cipher
            .decrypt(
                (&nonce).into(),
                Payload {
                    msg: &ciphertext,
                    aad: &aad,
                },
            )
            .map_err(|_| VapidUnsealErrorKind::AeadOpenFailed)?;
        Ok(Zeroizing::new(plaintext))
    }
}

fn build_aad(label: &[u8], kid_bytes: &[u8]) -> Vec<u8> {
    let mut aad = Vec::with_capacity(2 + label.len() + 2 + kid_bytes.len());
    aad.extend_from_slice(&(label.len() as u16).to_be_bytes());
    aad.extend_from_slice(label);
    aad.extend_from_slice(&(kid_bytes.len() as u16).to_be_bytes());
    aad.extend_from_slice(kid_bytes);
    aad
}

fn parse_env_scalar(raw: &str) -> Result<p256::SecretKey, VapidLoadError> {
    let raw = raw.trim();
    // Wrap the decoded bytes in Zeroizing so the raw P-256 scalar zeroes
    // out of heap when `bytes` falls out of scope, regardless of whether
    // `from_slice` succeeds.
    let bytes = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(raw.trim_end_matches('='))
            // DecodeError can include the offending input byte. Do not retain
            // any part of a malformed bootstrap secret in the error chain.
            .map_err(|_| VapidEnvParseError::Base64urlDecode)?,
    );
    if bytes.len() != 32 {
        return Err(VapidEnvParseError::InvalidLength { found: bytes.len() }.into());
    }
    p256::SecretKey::from_slice(bytes.as_slice())
        .map_err(|source| VapidEnvParseError::InvalidScalar { source }.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn root_key() -> &'static [u8; 32] {
        static KEY: std::sync::LazyLock<[u8; 32]> = std::sync::LazyLock::new(rand::random);
        &KEY
    }

    #[test]
    fn build_aad_is_length_prefixed() {
        let aad = build_aad(b"label", b"kid_bytes");
        // u16_be(5) || "label" || u16_be(9) || "kid_bytes"
        assert_eq!(&aad[..2], &5u16.to_be_bytes());
        assert_eq!(&aad[2..7], b"label");
        assert_eq!(&aad[7..9], &9u16.to_be_bytes());
        assert_eq!(&aad[9..], b"kid_bytes");
    }

    #[test]
    fn build_aad_prevents_canonicalization_collision() {
        // "vapid" + "-extra" vs "vapid-" + "extra" — same byte sequence
        // without length-prefixing, but our length-prefixed AAD differs.
        let a = build_aad(b"vapid", b"-extra");
        let b = build_aad(b"vapid-", b"extra");
        assert_ne!(a, b);
    }

    #[test]
    fn cipher_round_trip() {
        let cipher = VapidKeyCipher::new(root_key());
        let plaintext = b"my-32-byte-p256-scalar-padding!!";
        let sealed = cipher.seal(plaintext, AAD_LABEL, b"some-kid").unwrap();
        let opened = cipher.open(&sealed, AAD_LABEL, b"some-kid").unwrap();
        assert_eq!(opened.as_slice(), plaintext);
    }

    #[test]
    fn cipher_rejects_mismatched_kid() {
        let cipher = VapidKeyCipher::new(root_key());
        let plaintext = b"my-32-byte-p256-scalar-padding!!";
        let sealed = cipher.seal(plaintext, AAD_LABEL, b"kid-a").unwrap();
        let err = cipher.open(&sealed, AAD_LABEL, b"kid-b").unwrap_err();
        assert_eq!(err, VapidUnsealErrorKind::AeadOpenFailed);
    }

    #[test]
    fn cipher_rejects_mismatched_label() {
        let cipher = VapidKeyCipher::new(root_key());
        let plaintext = b"my-32-byte-p256-scalar-padding!!";
        let sealed = cipher.seal(plaintext, b"label-a", b"kid").unwrap();
        let err = cipher.open(&sealed, b"label-b", b"kid").unwrap_err();
        assert_eq!(err, VapidUnsealErrorKind::AeadOpenFailed);
    }

    #[test]
    fn cipher_rejects_wrong_root_key() {
        let cipher_a = VapidKeyCipher::new(root_key());
        let mut other_root_key = *root_key();
        other_root_key[0] ^= 1;
        let cipher_b = VapidKeyCipher::new(&other_root_key);
        let plaintext = b"my-32-byte-p256-scalar-padding!!";
        let sealed = cipher_a.seal(plaintext, AAD_LABEL, b"kid").unwrap();
        let err = cipher_b.open(&sealed, AAD_LABEL, b"kid").unwrap_err();
        assert_eq!(err, VapidUnsealErrorKind::AeadOpenFailed);
    }

    #[test]
    fn parse_env_scalar_accepts_base64url_no_pad_32_bytes() {
        let bytes = [0xABu8; 32];
        let encoded = URL_SAFE_NO_PAD.encode(bytes);
        let key = parse_env_scalar(&encoded).expect("valid scalar");
        assert_eq!(&key.to_bytes()[..], &bytes[..]);
    }

    #[test]
    fn parse_env_scalar_rejects_wrong_length() {
        let encoded = URL_SAFE_NO_PAD.encode([0u8; 16]);
        let err = parse_env_scalar(&encoded).unwrap_err();
        assert!(matches!(
            err,
            VapidLoadError::EnvParse(VapidEnvParseError::InvalidLength { found: 16 })
        ));
    }

    #[test]
    fn malformed_environment_error_does_not_retain_secret_bytes() {
        let secret = "very-private-key!";
        let error = parse_env_scalar(secret).unwrap_err();
        assert!(matches!(
            &error,
            VapidLoadError::EnvParse(VapidEnvParseError::Base64urlDecode)
        ));
        let debug = format!("{error:?}");
        assert!(!debug.contains(secret));
        assert!(!debug.contains("InvalidByte"));
    }

    #[test]
    fn parse_env_scalar_accepts_leading_zero() {
        let mut bytes = [0u8; 32];
        bytes[31] = 1; // scalar = 1 (smallest valid)
        let encoded = URL_SAFE_NO_PAD.encode(bytes);
        let _ = parse_env_scalar(&encoded).expect("leading-zero scalar is valid");
    }
}
