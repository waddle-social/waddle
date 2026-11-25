//! PostgreSQL (`sqlx::PgPool`) and SQLite-compat in-process contract
//! implementations of [`DestructionLedgerStore`].

use async_trait::async_trait;
use sqlx::{PgPool, Row, SqlitePool};
use uuid::Uuid;
use waddle_broker_core::identity::{AccountId, TenantId};

use crate::error::LedgerError;
use crate::ledger::{
    DestructionLedgerStore, DestructionReason, DestructionRecord, DestructionTargetKind,
    EgressGateState, EnrollmentNonceRecord, RetiredAccountRecord, StoredEnvelopeRecord,
};

const PG_SCHEMA: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS broker_destruction_ledger (
        entry_id TEXT PRIMARY KEY,
        target_kind TEXT NOT NULL CHECK (target_kind IN ('credential', 'credential_version', 'account', 'tenant', 'kms_key')),
        tenant_id TEXT NOT NULL,
        target_id TEXT NOT NULL,
        reason TEXT NOT NULL,
        destroyed_at_ms BIGINT NOT NULL,
        UNIQUE (target_kind, tenant_id, target_id)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS broker_retired_accounts (
        tenant_id TEXT NOT NULL,
        account_id TEXT NOT NULL,
        jid_key TEXT NOT NULL,
        localpart_key TEXT NOT NULL,
        retired_at_ms BIGINT NOT NULL,
        PRIMARY KEY (tenant_id, account_id)
    )"#,
    r#"CREATE INDEX IF NOT EXISTS idx_broker_retired_accounts_jid
        ON broker_retired_accounts (tenant_id, jid_key)"#,
    r#"CREATE TABLE IF NOT EXISTS broker_enrollment_nonces (
        nonce_sha256 BYTEA PRIMARY KEY,
        payload_digest_sha256 BYTEA NOT NULL,
        tenant_id TEXT NOT NULL,
        credential_id TEXT NOT NULL,
        credential_version INTEGER NOT NULL,
        expires_at_ms BIGINT NOT NULL,
        created_at_ms BIGINT NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS broker_egress_state (
        singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
        egress_open BOOLEAN NOT NULL,
        last_restore_applied_ms BIGINT
    )"#,
    r#"INSERT INTO broker_egress_state (singleton_id, egress_open, last_restore_applied_ms)
       VALUES (1, TRUE, NULL) ON CONFLICT (singleton_id) DO NOTHING"#,
    r#"CREATE TABLE IF NOT EXISTS credential_envelopes (
        credential_id TEXT PRIMARY KEY,
        tenant_id TEXT NOT NULL,
        account_id TEXT NOT NULL DEFAULT '',
        provider TEXT NOT NULL,
        version INTEGER NOT NULL,
        record_json TEXT NOT NULL,
        created_at_ms BIGINT NOT NULL
    )"#,
];

/// PostgreSQL [`DestructionLedgerStore`] backed by `sqlx::PgPool`.
pub struct PostgresLedgerStore {
    pool: PgPool,
}

impl PostgresLedgerStore {
    pub async fn new(pool: PgPool) -> Result<Self, LedgerError> {
        for stmt in PG_SCHEMA {
            sqlx::query(stmt)
                .execute(&pool)
                .await
                .map_err(|err| LedgerError::Backend(format!("pg schema: {err}")))?;
        }
        Ok(Self { pool })
    }
}

#[async_trait]
impl DestructionLedgerStore for PostgresLedgerStore {
    async fn append_destruction(&self, record: &DestructionRecord) -> Result<(), LedgerError> {
        sqlx::query(
            "INSERT INTO broker_destruction_ledger
             (entry_id, target_kind, tenant_id, target_id, reason, destroyed_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (target_kind, tenant_id, target_id) DO NOTHING",
        )
        .bind(record.entry_id.to_string())
        .bind(record.target_kind.as_str())
        .bind(record.tenant_id.as_str())
        .bind(&record.target_id)
        .bind(record.reason.as_str())
        .bind(record.destroyed_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(())
    }

    async fn list_destructions(&self) -> Result<Vec<DestructionRecord>, LedgerError> {
        let rows = sqlx::query(
            "SELECT entry_id, target_kind, tenant_id, target_id, reason, destroyed_at_ms
             FROM broker_destruction_ledger
             ORDER BY destroyed_at_ms ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;

        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let entry_id_str: String = row.get(0);
            let kind_str: String = row.get(1);
            let tenant_str: String = row.get(2);
            let target_id: String = row.get(3);
            let reason_str: String = row.get(4);
            let destroyed_at_ms: i64 = row.get(5);
            out.push(DestructionRecord {
                entry_id: Uuid::parse_str(&entry_id_str)
                    .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
                target_kind: DestructionTargetKind::parse(&kind_str)?,
                tenant_id: TenantId::parse(&tenant_str)
                    .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
                target_id,
                reason: DestructionReason::parse(&reason_str)?,
                destroyed_at_ms,
            });
        }
        Ok(out)
    }

    async fn is_destroyed(
        &self,
        kind: DestructionTargetKind,
        tenant_id: &TenantId,
        target_id: &str,
    ) -> Result<bool, LedgerError> {
        let found = sqlx::query(
            "SELECT 1 FROM broker_destruction_ledger
             WHERE target_kind = $1 AND tenant_id = $2 AND target_id = $3
             LIMIT 1",
        )
        .bind(kind.as_str())
        .bind(tenant_id.as_str())
        .bind(target_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(found.is_some())
    }

    async fn record_retired_account(
        &self,
        record: &RetiredAccountRecord,
    ) -> Result<(), LedgerError> {
        sqlx::query(
            "INSERT INTO broker_retired_accounts
             (tenant_id, account_id, jid_key, localpart_key, retired_at_ms)
             VALUES ($1, $2, $3, $4, $5)
             ON CONFLICT (tenant_id, account_id) DO UPDATE SET
               jid_key = EXCLUDED.jid_key,
               localpart_key = EXCLUDED.localpart_key,
               retired_at_ms = EXCLUDED.retired_at_ms",
        )
        .bind(record.tenant_id.as_str())
        .bind(record.account_id.to_string())
        .bind(&record.jid_key)
        .bind(&record.localpart_key)
        .bind(record.retired_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(())
    }

    async fn is_account_retired(
        &self,
        tenant_id: &TenantId,
        account_id: AccountId,
    ) -> Result<bool, LedgerError> {
        let found = sqlx::query(
            "SELECT 1 FROM broker_retired_accounts
             WHERE tenant_id = $1 AND account_id = $2 LIMIT 1",
        )
        .bind(tenant_id.as_str())
        .bind(account_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(found.is_some())
    }

    async fn is_jid_retired(
        &self,
        tenant_id: &TenantId,
        jid_key: &str,
    ) -> Result<bool, LedgerError> {
        let found = sqlx::query(
            "SELECT 1 FROM broker_retired_accounts
             WHERE tenant_id = $1 AND jid_key = $2 LIMIT 1",
        )
        .bind(tenant_id.as_str())
        .bind(jid_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(found.is_some())
    }

    async fn list_retired_accounts(&self) -> Result<Vec<RetiredAccountRecord>, LedgerError> {
        let rows = sqlx::query(
            "SELECT tenant_id, account_id, jid_key, localpart_key, retired_at_ms
             FROM broker_retired_accounts",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let tenant_str: String = row.get(0);
            let account_str: String = row.get(1);
            out.push(RetiredAccountRecord {
                tenant_id: TenantId::parse(&tenant_str)
                    .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
                account_id: AccountId::from_uuid(
                    Uuid::parse_str(&account_str)
                        .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
                ),
                jid_key: row.get(2),
                localpart_key: row.get(3),
                retired_at_ms: row.get(4),
            });
        }
        Ok(out)
    }

    async fn get_enrollment_nonce(
        &self,
        nonce_sha256: &[u8; 32],
    ) -> Result<Option<EnrollmentNonceRecord>, LedgerError> {
        let row = sqlx::query(
            "SELECT nonce_sha256, payload_digest_sha256, tenant_id, credential_id,
                    credential_version, expires_at_ms, created_at_ms
             FROM broker_enrollment_nonces WHERE nonce_sha256 = $1 LIMIT 1",
        )
        .bind(nonce_sha256.as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let n_bytes: Vec<u8> = row.get(0);
        let d_bytes: Vec<u8> = row.get(1);
        let tenant_str: String = row.get(2);
        let cred_str: String = row.get(3);
        let version_i32: i32 = row.get(4);
        Ok(Some(EnrollmentNonceRecord {
            nonce_sha256: n_bytes
                .try_into()
                .map_err(|_| LedgerError::InvalidState("invalid nonce_sha256".to_owned()))?,
            payload_digest_sha256: d_bytes
                .try_into()
                .map_err(|_| LedgerError::InvalidState("invalid payload_digest".to_owned()))?,
            tenant_id: TenantId::parse(&tenant_str)
                .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
            credential_id: Uuid::parse_str(&cred_str)
                .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
            credential_version: u32::try_from(version_i32)
                .map_err(|_| LedgerError::InvalidState("invalid version".to_owned()))?,
            expires_at_ms: row.get(5),
            created_at_ms: row.get(6),
        }))
    }

    async fn put_enrollment_nonce(
        &self,
        record: &EnrollmentNonceRecord,
    ) -> Result<(), LedgerError> {
        let version_i32 = i32::try_from(record.credential_version)
            .map_err(|_| LedgerError::InvalidState("version overflow".to_owned()))?;
        sqlx::query(
            "INSERT INTO broker_enrollment_nonces
             (nonce_sha256, payload_digest_sha256, tenant_id, credential_id,
              credential_version, expires_at_ms, created_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (nonce_sha256) DO NOTHING",
        )
        .bind(record.nonce_sha256.as_slice())
        .bind(record.payload_digest_sha256.as_slice())
        .bind(record.tenant_id.as_str())
        .bind(record.credential_id.to_string())
        .bind(version_i32)
        .bind(record.expires_at_ms)
        .bind(record.created_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(())
    }

    async fn get_egress_state(&self) -> Result<EgressGateState, LedgerError> {
        let row = sqlx::query(
            "SELECT egress_open, last_restore_applied_ms
             FROM broker_egress_state WHERE singleton_id = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        let Some(row) = row else {
            return Ok(EgressGateState {
                egress_open: false,
                last_restore_applied_ms: None,
            });
        };
        Ok(EgressGateState {
            egress_open: row.get(0),
            last_restore_applied_ms: row.get(1),
        })
    }

    async fn set_egress_state(&self, state: EgressGateState) -> Result<(), LedgerError> {
        sqlx::query(
            "UPDATE broker_egress_state
             SET egress_open = $1, last_restore_applied_ms = COALESCE($2, last_restore_applied_ms)
             WHERE singleton_id = 1",
        )
        .bind(state.egress_open)
        .bind(state.last_restore_applied_ms)
        .execute(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(())
    }

    async fn put_envelope(&self, record: &StoredEnvelopeRecord) -> Result<(), LedgerError> {
        let json = serde_json::to_string(record)
            .map_err(|err| LedgerError::Serialization(err.to_string()))?;
        let version_i32 = i32::try_from(record.version)
            .map_err(|_| LedgerError::InvalidState("version overflow".to_owned()))?;
        sqlx::query(
            "INSERT INTO credential_envelopes
             (credential_id, tenant_id, account_id, provider, version, record_json, created_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (credential_id) DO UPDATE SET
               version = EXCLUDED.version,
               record_json = EXCLUDED.record_json,
               created_at_ms = EXCLUDED.created_at_ms",
        )
        .bind(record.credential_id.to_string())
        .bind(record.tenant_id.as_str())
        .bind(record.account_id.map(|a| a.to_string()).unwrap_or_default())
        .bind(&record.provider)
        .bind(version_i32)
        .bind(json)
        .bind(record.created_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(())
    }

    async fn get_envelope(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
    ) -> Result<Option<StoredEnvelopeRecord>, LedgerError> {
        let row = sqlx::query(
            "SELECT record_json FROM credential_envelopes
             WHERE tenant_id = $1 AND credential_id = $2 LIMIT 1",
        )
        .bind(tenant_id.as_str())
        .bind(credential_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let json: String = row.get(0);
        let record: StoredEnvelopeRecord = serde_json::from_str(&json)
            .map_err(|err| LedgerError::Serialization(err.to_string()))?;
        Ok(Some(record))
    }

    async fn list_envelopes_for_tenant(
        &self,
        tenant_id: &TenantId,
    ) -> Result<Vec<StoredEnvelopeRecord>, LedgerError> {
        let rows = sqlx::query("SELECT record_json FROM credential_envelopes WHERE tenant_id = $1")
            .bind(tenant_id.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|err| LedgerError::Backend(err.to_string()))?;
        rows.into_iter()
            .map(|r| {
                let json: String = r.get(0);
                serde_json::from_str(&json)
                    .map_err(|err| LedgerError::Serialization(err.to_string()))
            })
            .collect()
    }

    async fn list_all_envelopes(&self) -> Result<Vec<StoredEnvelopeRecord>, LedgerError> {
        let rows = sqlx::query("SELECT record_json FROM credential_envelopes")
            .fetch_all(&self.pool)
            .await
            .map_err(|err| LedgerError::Backend(err.to_string()))?;
        rows.into_iter()
            .map(|r| {
                let json: String = r.get(0);
                serde_json::from_str(&json)
                    .map_err(|err| LedgerError::Serialization(err.to_string()))
            })
            .collect()
    }

    async fn delete_envelope(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
    ) -> Result<bool, LedgerError> {
        let res = sqlx::query(
            "DELETE FROM credential_envelopes WHERE tenant_id = $1 AND credential_id = $2",
        )
        .bind(tenant_id.as_str())
        .bind(credential_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(res.rows_affected() > 0)
    }

    async fn delete_envelopes_for_account(
        &self,
        tenant_id: &TenantId,
        account_id: AccountId,
    ) -> Result<u64, LedgerError> {
        let res = sqlx::query(
            "DELETE FROM credential_envelopes WHERE tenant_id = $1 AND account_id = $2",
        )
        .bind(tenant_id.as_str())
        .bind(account_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(res.rows_affected())
    }

    async fn delete_envelopes_for_tenant(&self, tenant_id: &TenantId) -> Result<u64, LedgerError> {
        let res = sqlx::query("DELETE FROM credential_envelopes WHERE tenant_id = $1")
            .bind(tenant_id.as_str())
            .execute(&self.pool)
            .await
            .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(res.rows_affected())
    }
}

const SQLITE_COMPAT_SCHEMA: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS broker_destruction_ledger (
        entry_id TEXT PRIMARY KEY,
        target_kind TEXT NOT NULL CHECK (target_kind IN ('credential', 'credential_version', 'account', 'tenant', 'kms_key')),
        tenant_id TEXT NOT NULL,
        target_id TEXT NOT NULL,
        reason TEXT NOT NULL,
        destroyed_at_ms INTEGER NOT NULL,
        UNIQUE (target_kind, tenant_id, target_id)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS broker_retired_accounts (
        tenant_id TEXT NOT NULL,
        account_id TEXT NOT NULL,
        jid_key TEXT NOT NULL,
        localpart_key TEXT NOT NULL,
        retired_at_ms INTEGER NOT NULL,
        PRIMARY KEY (tenant_id, account_id)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS broker_enrollment_nonces (
        nonce_sha256 BLOB PRIMARY KEY,
        payload_digest_sha256 BLOB NOT NULL,
        tenant_id TEXT NOT NULL,
        credential_id TEXT NOT NULL,
        credential_version INTEGER NOT NULL,
        expires_at_ms INTEGER NOT NULL,
        created_at_ms INTEGER NOT NULL
    )"#,
    r#"CREATE TABLE IF NOT EXISTS broker_egress_state (
        singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
        egress_open INTEGER NOT NULL CHECK (egress_open IN (0, 1)),
        last_restore_applied_ms INTEGER
    )"#,
    r#"INSERT OR IGNORE INTO broker_egress_state (singleton_id, egress_open, last_restore_applied_ms)
       VALUES (1, 1, NULL)"#,
    r#"CREATE TABLE IF NOT EXISTS credential_envelopes (
        credential_id TEXT PRIMARY KEY,
        tenant_id TEXT NOT NULL,
        account_id TEXT NOT NULL DEFAULT '',
        provider TEXT NOT NULL,
        version INTEGER NOT NULL,
        record_json TEXT NOT NULL,
        created_at_ms INTEGER NOT NULL
    )"#,
];

/// SQLite-compat in-process contract store matching [`PostgresLedgerStore`] SQL semantics.
pub struct SqliteCompatLedgerStore {
    pool: SqlitePool,
}

impl SqliteCompatLedgerStore {
    pub async fn connect_in_memory() -> Result<Self, LedgerError> {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Self::new(pool).await
    }

    pub async fn new(pool: SqlitePool) -> Result<Self, LedgerError> {
        for stmt in SQLITE_COMPAT_SCHEMA {
            sqlx::query(stmt)
                .execute(&pool)
                .await
                .map_err(|err| LedgerError::Backend(err.to_string()))?;
        }
        Ok(Self { pool })
    }

    /// Snapshots all active `credential_envelopes` rows for restore-drill testing.
    pub async fn snapshot_envelopes(&self) -> Result<Vec<StoredEnvelopeRecord>, LedgerError> {
        self.list_all_envelopes().await
    }

    /// Replaces `credential_envelopes` with a stale backup snapshot while
    /// preserving `broker_destruction_ledger` and `broker_retired_accounts`.
    pub async fn restore_envelopes_snapshot(
        &self,
        snapshot: &[StoredEnvelopeRecord],
    ) -> Result<(), LedgerError> {
        sqlx::query("DELETE FROM credential_envelopes")
            .execute(&self.pool)
            .await
            .map_err(|err| LedgerError::Backend(err.to_string()))?;
        for rec in snapshot {
            self.put_envelope(rec).await?;
        }
        Ok(())
    }
}

#[async_trait]
impl DestructionLedgerStore for SqliteCompatLedgerStore {
    async fn append_destruction(&self, record: &DestructionRecord) -> Result<(), LedgerError> {
        sqlx::query(
            "INSERT OR IGNORE INTO broker_destruction_ledger
             (entry_id, target_kind, tenant_id, target_id, reason, destroyed_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )
        .bind(record.entry_id.to_string())
        .bind(record.target_kind.as_str())
        .bind(record.tenant_id.as_str())
        .bind(&record.target_id)
        .bind(record.reason.as_str())
        .bind(record.destroyed_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(())
    }

    async fn list_destructions(&self) -> Result<Vec<DestructionRecord>, LedgerError> {
        let rows = sqlx::query(
            "SELECT entry_id, target_kind, tenant_id, target_id, reason, destroyed_at_ms
             FROM broker_destruction_ledger ORDER BY destroyed_at_ms ASC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let entry_id_str: String = row.get(0);
            let kind_str: String = row.get(1);
            let tenant_str: String = row.get(2);
            let target_id: String = row.get(3);
            let reason_str: String = row.get(4);
            let destroyed_at_ms: i64 = row.get(5);
            out.push(DestructionRecord {
                entry_id: Uuid::parse_str(&entry_id_str)
                    .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
                target_kind: DestructionTargetKind::parse(&kind_str)?,
                tenant_id: TenantId::parse(&tenant_str)
                    .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
                target_id,
                reason: DestructionReason::parse(&reason_str)?,
                destroyed_at_ms,
            });
        }
        Ok(out)
    }

    async fn is_destroyed(
        &self,
        kind: DestructionTargetKind,
        tenant_id: &TenantId,
        target_id: &str,
    ) -> Result<bool, LedgerError> {
        let found = sqlx::query(
            "SELECT 1 FROM broker_destruction_ledger
             WHERE target_kind = ?1 AND tenant_id = ?2 AND target_id = ?3 LIMIT 1",
        )
        .bind(kind.as_str())
        .bind(tenant_id.as_str())
        .bind(target_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(found.is_some())
    }

    async fn record_retired_account(
        &self,
        record: &RetiredAccountRecord,
    ) -> Result<(), LedgerError> {
        sqlx::query(
            "INSERT OR REPLACE INTO broker_retired_accounts
             (tenant_id, account_id, jid_key, localpart_key, retired_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .bind(record.tenant_id.as_str())
        .bind(record.account_id.to_string())
        .bind(&record.jid_key)
        .bind(&record.localpart_key)
        .bind(record.retired_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(())
    }

    async fn is_account_retired(
        &self,
        tenant_id: &TenantId,
        account_id: AccountId,
    ) -> Result<bool, LedgerError> {
        let found = sqlx::query(
            "SELECT 1 FROM broker_retired_accounts
             WHERE tenant_id = ?1 AND account_id = ?2 LIMIT 1",
        )
        .bind(tenant_id.as_str())
        .bind(account_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(found.is_some())
    }

    async fn is_jid_retired(
        &self,
        tenant_id: &TenantId,
        jid_key: &str,
    ) -> Result<bool, LedgerError> {
        let found = sqlx::query(
            "SELECT 1 FROM broker_retired_accounts
             WHERE tenant_id = ?1 AND jid_key = ?2 LIMIT 1",
        )
        .bind(tenant_id.as_str())
        .bind(jid_key)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(found.is_some())
    }

    async fn list_retired_accounts(&self) -> Result<Vec<RetiredAccountRecord>, LedgerError> {
        let rows = sqlx::query(
            "SELECT tenant_id, account_id, jid_key, localpart_key, retired_at_ms
             FROM broker_retired_accounts",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let tenant_str: String = row.get(0);
            let account_str: String = row.get(1);
            out.push(RetiredAccountRecord {
                tenant_id: TenantId::parse(&tenant_str)
                    .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
                account_id: AccountId::from_uuid(
                    Uuid::parse_str(&account_str)
                        .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
                ),
                jid_key: row.get(2),
                localpart_key: row.get(3),
                retired_at_ms: row.get(4),
            });
        }
        Ok(out)
    }

    async fn get_enrollment_nonce(
        &self,
        nonce_sha256: &[u8; 32],
    ) -> Result<Option<EnrollmentNonceRecord>, LedgerError> {
        let row = sqlx::query(
            "SELECT nonce_sha256, payload_digest_sha256, tenant_id, credential_id,
                    credential_version, expires_at_ms, created_at_ms
             FROM broker_enrollment_nonces WHERE nonce_sha256 = ?1 LIMIT 1",
        )
        .bind(nonce_sha256.as_slice())
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let n_bytes: Vec<u8> = row.get(0);
        let d_bytes: Vec<u8> = row.get(1);
        let tenant_str: String = row.get(2);
        let cred_str: String = row.get(3);
        let version_i64: i64 = row.get(4);
        Ok(Some(EnrollmentNonceRecord {
            nonce_sha256: n_bytes
                .try_into()
                .map_err(|_| LedgerError::InvalidState("invalid nonce_sha256".to_owned()))?,
            payload_digest_sha256: d_bytes
                .try_into()
                .map_err(|_| LedgerError::InvalidState("invalid payload_digest".to_owned()))?,
            tenant_id: TenantId::parse(&tenant_str)
                .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
            credential_id: Uuid::parse_str(&cred_str)
                .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
            credential_version: u32::try_from(version_i64)
                .map_err(|_| LedgerError::InvalidState("invalid version".to_owned()))?,
            expires_at_ms: row.get(5),
            created_at_ms: row.get(6),
        }))
    }

    async fn put_enrollment_nonce(
        &self,
        record: &EnrollmentNonceRecord,
    ) -> Result<(), LedgerError> {
        sqlx::query(
            "INSERT OR IGNORE INTO broker_enrollment_nonces
             (nonce_sha256, payload_digest_sha256, tenant_id, credential_id,
              credential_version, expires_at_ms, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(record.nonce_sha256.as_slice())
        .bind(record.payload_digest_sha256.as_slice())
        .bind(record.tenant_id.as_str())
        .bind(record.credential_id.to_string())
        .bind(i64::from(record.credential_version))
        .bind(record.expires_at_ms)
        .bind(record.created_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(())
    }

    async fn get_egress_state(&self) -> Result<EgressGateState, LedgerError> {
        let row = sqlx::query(
            "SELECT egress_open, last_restore_applied_ms
             FROM broker_egress_state WHERE singleton_id = 1",
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        let Some(row) = row else {
            return Ok(EgressGateState {
                egress_open: false,
                last_restore_applied_ms: None,
            });
        };
        let open_int: i64 = row.get(0);
        Ok(EgressGateState {
            egress_open: open_int == 1,
            last_restore_applied_ms: row.get(1),
        })
    }

    async fn set_egress_state(&self, state: EgressGateState) -> Result<(), LedgerError> {
        let open_int: i64 = if state.egress_open { 1 } else { 0 };
        sqlx::query(
            "UPDATE broker_egress_state
             SET egress_open = ?1, last_restore_applied_ms = COALESCE(?2, last_restore_applied_ms)
             WHERE singleton_id = 1",
        )
        .bind(open_int)
        .bind(state.last_restore_applied_ms)
        .execute(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(())
    }

    async fn put_envelope(&self, record: &StoredEnvelopeRecord) -> Result<(), LedgerError> {
        let json = serde_json::to_string(record)
            .map_err(|err| LedgerError::Serialization(err.to_string()))?;
        sqlx::query(
            "INSERT OR REPLACE INTO credential_envelopes
             (credential_id, tenant_id, account_id, provider, version, record_json, created_at_ms)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .bind(record.credential_id.to_string())
        .bind(record.tenant_id.as_str())
        .bind(record.account_id.map(|a| a.to_string()).unwrap_or_default())
        .bind(&record.provider)
        .bind(i64::from(record.version))
        .bind(json)
        .bind(record.created_at_ms)
        .execute(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(())
    }

    async fn get_envelope(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
    ) -> Result<Option<StoredEnvelopeRecord>, LedgerError> {
        let row = sqlx::query(
            "SELECT record_json FROM credential_envelopes
             WHERE tenant_id = ?1 AND credential_id = ?2 LIMIT 1",
        )
        .bind(tenant_id.as_str())
        .bind(credential_id.to_string())
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        let Some(row) = row else {
            return Ok(None);
        };
        let json: String = row.get(0);
        let record: StoredEnvelopeRecord = serde_json::from_str(&json)
            .map_err(|err| LedgerError::Serialization(err.to_string()))?;
        Ok(Some(record))
    }

    async fn list_envelopes_for_tenant(
        &self,
        tenant_id: &TenantId,
    ) -> Result<Vec<StoredEnvelopeRecord>, LedgerError> {
        let rows = sqlx::query("SELECT record_json FROM credential_envelopes WHERE tenant_id = ?1")
            .bind(tenant_id.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(|err| LedgerError::Backend(err.to_string()))?;
        rows.into_iter()
            .map(|r| {
                let json: String = r.get(0);
                serde_json::from_str(&json)
                    .map_err(|err| LedgerError::Serialization(err.to_string()))
            })
            .collect()
    }

    async fn list_all_envelopes(&self) -> Result<Vec<StoredEnvelopeRecord>, LedgerError> {
        let rows = sqlx::query("SELECT record_json FROM credential_envelopes")
            .fetch_all(&self.pool)
            .await
            .map_err(|err| LedgerError::Backend(err.to_string()))?;
        rows.into_iter()
            .map(|r| {
                let json: String = r.get(0);
                serde_json::from_str(&json)
                    .map_err(|err| LedgerError::Serialization(err.to_string()))
            })
            .collect()
    }

    async fn delete_envelope(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
    ) -> Result<bool, LedgerError> {
        let res = sqlx::query(
            "DELETE FROM credential_envelopes WHERE tenant_id = ?1 AND credential_id = ?2",
        )
        .bind(tenant_id.as_str())
        .bind(credential_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(res.rows_affected() > 0)
    }

    async fn delete_envelopes_for_account(
        &self,
        tenant_id: &TenantId,
        account_id: AccountId,
    ) -> Result<u64, LedgerError> {
        let res = sqlx::query(
            "DELETE FROM credential_envelopes WHERE tenant_id = ?1 AND account_id = ?2",
        )
        .bind(tenant_id.as_str())
        .bind(account_id.to_string())
        .execute(&self.pool)
        .await
        .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(res.rows_affected())
    }

    async fn delete_envelopes_for_tenant(&self, tenant_id: &TenantId) -> Result<u64, LedgerError> {
        let res = sqlx::query("DELETE FROM credential_envelopes WHERE tenant_id = ?1")
            .bind(tenant_id.as_str())
            .execute(&self.pool)
            .await
            .map_err(|err| LedgerError::Backend(err.to_string()))?;
        Ok(res.rows_affected())
    }
}
