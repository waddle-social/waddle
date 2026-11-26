//! Turso WAL-mode [`DestructionLedgerStore`] (`PRAGMA journal_mode = WAL`,
//! `PRAGMA synchronous = FULL`, `BEGIN IMMEDIATE`).
//!
//! Separates the encrypted credential vault DB (`vault.db`) from the
//! append-only destruction ledger DB (`destruction_ledger.db`) so restoring a
//! stale vault backup snapshot never erases destruction tombstones.

use std::path::{Path, PathBuf};

use async_trait::async_trait;
use tokio::sync::Mutex;
use uuid::Uuid;
use waddle_broker_core::identity::{AccountId, TenantId};

use crate::error::LedgerError;
use crate::ledger::{
    DestructionLedgerStore, DestructionReason, DestructionRecord, DestructionTargetKind,
    EgressGateState, EnrollmentNonceRecord, NonceClaimOutcome, RetiredAccountRecord,
    StoredEnvelopeRecord,
};

const LEDGER_SCHEMA: &[&str] = &[
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
    r#"CREATE INDEX IF NOT EXISTS idx_broker_retired_accounts_jid
        ON broker_retired_accounts (tenant_id, jid_key)"#,
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
];

const VAULT_SCHEMA: &str = r#"CREATE TABLE IF NOT EXISTS credential_envelopes (
    credential_id TEXT PRIMARY KEY,
    tenant_id TEXT NOT NULL,
    account_id TEXT,
    provider TEXT NOT NULL,
    version INTEGER NOT NULL,
    record_json TEXT NOT NULL,
    created_at_ms INTEGER NOT NULL
)"#;

struct TursoDbHandle {
    _db: turso::Database,
    conn: turso::Connection,
}

async fn open_wal_db(path: &Path, ddl: &[&str]) -> Result<TursoDbHandle, LedgerError> {
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|err| LedgerError::Backend(format!("create db dir failed: {err}")))?;
    }
    let path_str = path
        .to_str()
        .ok_or_else(|| LedgerError::Backend("non-UTF-8 DB path".to_owned()))?;
    let db = turso::Builder::new_local(path_str)
        .build()
        .await
        .map_err(|err| LedgerError::Backend(format!("turso open failed: {err}")))?;
    let conn = db
        .connect()
        .map_err(|err| LedgerError::Backend(format!("turso connect failed: {err}")))?;
    let _ = conn
        .pragma_update("journal_mode", "WAL")
        .await
        .map_err(|err| LedgerError::Backend(format!("WAL pragma failed: {err}")))?;
    let _ = conn
        .pragma_update("synchronous", "FULL")
        .await
        .map_err(|err| LedgerError::Backend(format!("synchronous=FULL pragma failed: {err}")))?;
    let _ = conn
        .pragma_update("foreign_keys", "ON")
        .await
        .map_err(|err| LedgerError::Backend(format!("foreign_keys pragma failed: {err}")))?;
    for stmt in ddl {
        conn.execute(stmt, ())
            .await
            .map_err(|err| LedgerError::Backend(format!("DDL failed: {err}")))?;
    }
    Ok(TursoDbHandle { _db: db, conn })
}

/// Turso WAL-mode durable ledger and encrypted credential vault store.
pub struct TursoLedgerStore {
    vault_path: PathBuf,
    ledger: Mutex<TursoDbHandle>,
    vault: Mutex<Option<TursoDbHandle>>,
}

impl TursoLedgerStore {
    /// Opens `<dir>/vault.db` and `<dir>/destruction_ledger.db` in WAL mode.
    pub async fn open_in_dir(dir: &Path) -> Result<Self, LedgerError> {
        Self::open_split(&dir.join("vault.db"), &dir.join("destruction_ledger.db")).await
    }

    /// Opens separate vault and destruction-ledger Turso WAL databases.
    pub async fn open_split(vault_path: &Path, ledger_path: &Path) -> Result<Self, LedgerError> {
        let ledger = open_wal_db(ledger_path, LEDGER_SCHEMA).await?;
        let vault = open_wal_db(vault_path, &[VAULT_SCHEMA]).await?;
        Ok(Self {
            vault_path: vault_path.to_path_buf(),
            ledger: Mutex::new(ledger),
            vault: Mutex::new(Some(vault)),
        })
    }

    pub fn vault_path(&self) -> &Path {
        &self.vault_path
    }

    /// Closes the vault handle so Turso flushes WAL/pages cleanly, copies
    /// `vault.db` (plus `-wal` if present) to `backup_path`, and reopens it.
    pub async fn snapshot_vault_to(&self, backup_path: &Path) -> Result<(), LedgerError> {
        let mut guard = self.vault.lock().await;
        if let Some(handle) = guard.as_ref() {
            if let Ok(mut rows) = handle
                .conn
                .query("PRAGMA wal_checkpoint(TRUNCATE)", ())
                .await
            {
                while let Ok(Some(_)) = rows.next().await {}
            }
        }
        // Explicitly drop connection & database so all file handles flush before copy.
        drop(guard.take());

        tokio::fs::copy(&self.vault_path, backup_path)
            .await
            .map_err(|err| LedgerError::Backend(format!("vault backup copy failed: {err}")))?;
        let src_wal = PathBuf::from(format!("{}-wal", self.vault_path.display()));
        let dst_wal = PathBuf::from(format!("{}-wal", backup_path.display()));
        if matches!(tokio::fs::try_exists(&src_wal).await, Ok(true)) {
            tokio::fs::copy(&src_wal, &dst_wal).await.map_err(|err| {
                LedgerError::Backend(format!("vault backup wal copy failed: {err}"))
            })?;
        } else if matches!(tokio::fs::try_exists(&dst_wal).await, Ok(true)) {
            let _ = tokio::fs::remove_file(&dst_wal).await;
        }

        let reopened = open_wal_db(&self.vault_path, &[VAULT_SCHEMA]).await?;
        *guard = Some(reopened);
        Ok(())
    }

    /// Closes the vault handle, replaces `vault.db` on disk with `backup_path`
    /// (simulating a basebackup + WAL restore), and reopens the vault
    /// connection while preserving the separate `destruction_ledger.db`.
    pub async fn replace_vault_from_backup(&self, backup_path: &Path) -> Result<(), LedgerError> {
        let mut guard = self.vault.lock().await;
        // Close existing vault DB handle before mutating files on disk.
        drop(guard.take());

        for suffix in ["-wal", "-shm"] {
            let sidecar = PathBuf::from(format!("{}{suffix}", self.vault_path.display()));
            if matches!(tokio::fs::try_exists(&sidecar).await, Ok(true)) {
                let _ = tokio::fs::remove_file(&sidecar).await;
            }
        }
        tokio::fs::copy(backup_path, &self.vault_path)
            .await
            .map_err(|err| LedgerError::Backend(format!("vault restore copy failed: {err}")))?;
        let backup_wal = PathBuf::from(format!("{}-wal", backup_path.display()));
        let dst_wal = PathBuf::from(format!("{}-wal", self.vault_path.display()));
        if matches!(tokio::fs::try_exists(&backup_wal).await, Ok(true)) {
            tokio::fs::copy(&backup_wal, &dst_wal)
                .await
                .map_err(|err| {
                    LedgerError::Backend(format!("vault restore wal copy failed: {err}"))
                })?;
        }
        let reopened = open_wal_db(&self.vault_path, &[VAULT_SCHEMA]).await?;
        *guard = Some(reopened);
        Ok(())
    }
}

fn col_text(row: &turso::Row, idx: usize) -> Result<String, LedgerError> {
    match row.get_value(idx) {
        Ok(turso::Value::Text(s)) => Ok(s),
        other => Err(LedgerError::InvalidState(format!(
            "expected text at col {idx}, got {other:?}"
        ))),
    }
}

fn col_i64(row: &turso::Row, idx: usize) -> Result<i64, LedgerError> {
    match row.get_value(idx) {
        Ok(turso::Value::Integer(n)) => Ok(n),
        other => Err(LedgerError::InvalidState(format!(
            "expected integer at col {idx}, got {other:?}"
        ))),
    }
}

fn col_opt_i64(row: &turso::Row, idx: usize) -> Result<Option<i64>, LedgerError> {
    match row.get_value(idx) {
        Ok(turso::Value::Integer(n)) => Ok(Some(n)),
        Ok(turso::Value::Null) => Ok(None),
        other => Err(LedgerError::InvalidState(format!(
            "expected optional integer at col {idx}, got {other:?}"
        ))),
    }
}

fn col_blob32(row: &turso::Row, idx: usize) -> Result<[u8; 32], LedgerError> {
    match row.get_value(idx) {
        Ok(turso::Value::Blob(bytes)) => bytes
            .try_into()
            .map_err(|_| LedgerError::InvalidState(format!("expected 32-byte blob at col {idx}"))),
        other => Err(LedgerError::InvalidState(format!(
            "expected blob at col {idx}, got {other:?}"
        ))),
    }
}

#[async_trait]
impl DestructionLedgerStore for TursoLedgerStore {
    async fn append_destruction(&self, record: &DestructionRecord) -> Result<(), LedgerError> {
        let guard = self.ledger.lock().await;
        guard
            .conn
            .execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|err| LedgerError::Backend(format!("BEGIN IMMEDIATE: {err}")))?;
        let res = guard
            .conn
            .execute(
                "INSERT OR IGNORE INTO broker_destruction_ledger
                 (entry_id, target_kind, tenant_id, target_id, reason, destroyed_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                (
                    record.entry_id.to_string(),
                    record.target_kind.as_str().to_owned(),
                    record.tenant_id.as_str().to_owned(),
                    record.target_id.clone(),
                    record.reason.as_str().to_owned(),
                    record.destroyed_at_ms,
                ),
            )
            .await;
        match res {
            Ok(_) => {
                guard
                    .conn
                    .execute("COMMIT", ())
                    .await
                    .map_err(|err| LedgerError::Backend(format!("COMMIT: {err}")))?;
                Ok(())
            }
            Err(err) => {
                let _ = guard.conn.execute("ROLLBACK", ()).await;
                Err(LedgerError::Backend(format!("append_destruction: {err}")))
            }
        }
    }

    async fn list_destructions(&self) -> Result<Vec<DestructionRecord>, LedgerError> {
        let guard = self.ledger.lock().await;
        let mut rows = guard
            .conn
            .query(
                "SELECT entry_id, target_kind, tenant_id, target_id, reason, destroyed_at_ms
                 FROM broker_destruction_ledger
                 ORDER BY destroyed_at_ms ASC",
                (),
            )
            .await
            .map_err(|err| LedgerError::Backend(format!("list_destructions: {err}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|err| LedgerError::Backend(format!("list_destructions next: {err}")))?
        {
            let entry_id = Uuid::parse_str(&col_text(&row, 0)?)
                .map_err(|err| LedgerError::InvalidState(err.to_string()))?;
            let target_kind = DestructionTargetKind::parse(&col_text(&row, 1)?)?;
            let tenant_id = TenantId::parse(&col_text(&row, 2)?)
                .map_err(|err| LedgerError::InvalidState(err.to_string()))?;
            let target_id = col_text(&row, 3)?;
            let reason = DestructionReason::parse(&col_text(&row, 4)?)?;
            let destroyed_at_ms = col_i64(&row, 5)?;
            out.push(DestructionRecord {
                entry_id,
                target_kind,
                tenant_id,
                target_id,
                reason,
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
        let guard = self.ledger.lock().await;
        let mut rows = guard
            .conn
            .query(
                "SELECT 1 FROM broker_destruction_ledger
                 WHERE target_kind = ?1 AND tenant_id = ?2 AND target_id = ?3
                 LIMIT 1",
                (
                    kind.as_str().to_owned(),
                    tenant_id.as_str().to_owned(),
                    target_id.to_owned(),
                ),
            )
            .await
            .map_err(|err| LedgerError::Backend(format!("is_destroyed: {err}")))?;
        let found = rows
            .next()
            .await
            .map_err(|err| LedgerError::Backend(format!("is_destroyed next: {err}")))?
            .is_some();
        Ok(found)
    }

    async fn record_retired_account(
        &self,
        record: &RetiredAccountRecord,
    ) -> Result<(), LedgerError> {
        let guard = self.ledger.lock().await;
        guard
            .conn
            .execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|err| LedgerError::Backend(format!("BEGIN IMMEDIATE: {err}")))?;
        let res = guard
            .conn
            .execute(
                "INSERT OR REPLACE INTO broker_retired_accounts
                 (tenant_id, account_id, jid_key, localpart_key, retired_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                (
                    record.tenant_id.as_str().to_owned(),
                    record.account_id.to_string(),
                    record.jid_key.clone(),
                    record.localpart_key.clone(),
                    record.retired_at_ms,
                ),
            )
            .await;
        match res {
            Ok(_) => {
                guard
                    .conn
                    .execute("COMMIT", ())
                    .await
                    .map_err(|err| LedgerError::Backend(format!("COMMIT: {err}")))?;
                Ok(())
            }
            Err(err) => {
                let _ = guard.conn.execute("ROLLBACK", ()).await;
                Err(LedgerError::Backend(format!(
                    "record_retired_account: {err}"
                )))
            }
        }
    }

    async fn is_account_retired(
        &self,
        tenant_id: &TenantId,
        account_id: AccountId,
    ) -> Result<bool, LedgerError> {
        let guard = self.ledger.lock().await;
        let mut rows = guard
            .conn
            .query(
                "SELECT 1 FROM broker_retired_accounts
                 WHERE tenant_id = ?1 AND account_id = ?2
                 LIMIT 1",
                (tenant_id.as_str().to_owned(), account_id.to_string()),
            )
            .await
            .map_err(|err| LedgerError::Backend(format!("is_account_retired: {err}")))?;
        let found = rows
            .next()
            .await
            .map_err(|err| LedgerError::Backend(format!("is_account_retired next: {err}")))?
            .is_some();
        Ok(found)
    }

    async fn is_jid_retired(
        &self,
        tenant_id: &TenantId,
        jid_key: &str,
    ) -> Result<bool, LedgerError> {
        let guard = self.ledger.lock().await;
        let mut rows = guard
            .conn
            .query(
                "SELECT 1 FROM broker_retired_accounts
                 WHERE tenant_id = ?1 AND jid_key = ?2
                 LIMIT 1",
                (tenant_id.as_str().to_owned(), jid_key.to_owned()),
            )
            .await
            .map_err(|err| LedgerError::Backend(format!("is_jid_retired: {err}")))?;
        let found = rows
            .next()
            .await
            .map_err(|err| LedgerError::Backend(format!("is_jid_retired next: {err}")))?
            .is_some();
        Ok(found)
    }

    async fn list_retired_accounts(&self) -> Result<Vec<RetiredAccountRecord>, LedgerError> {
        let guard = self.ledger.lock().await;
        let mut rows = guard
            .conn
            .query(
                "SELECT tenant_id, account_id, jid_key, localpart_key, retired_at_ms
                 FROM broker_retired_accounts",
                (),
            )
            .await
            .map_err(|err| LedgerError::Backend(format!("list_retired_accounts: {err}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|err| LedgerError::Backend(format!("list_retired_accounts next: {err}")))?
        {
            let tenant_id = TenantId::parse(&col_text(&row, 0)?)
                .map_err(|err| LedgerError::InvalidState(err.to_string()))?;
            let account_id = AccountId::from_uuid(
                Uuid::parse_str(&col_text(&row, 1)?)
                    .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
            );
            out.push(RetiredAccountRecord {
                tenant_id,
                account_id,
                jid_key: col_text(&row, 2)?,
                localpart_key: col_text(&row, 3)?,
                retired_at_ms: col_i64(&row, 4)?,
            });
        }
        Ok(out)
    }

    async fn get_enrollment_nonce(
        &self,
        nonce_sha256: &[u8; 32],
    ) -> Result<Option<EnrollmentNonceRecord>, LedgerError> {
        let guard = self.ledger.lock().await;
        let mut rows = guard
            .conn
            .query(
                "SELECT nonce_sha256, payload_digest_sha256, tenant_id, credential_id,
                        credential_version, expires_at_ms, created_at_ms
                 FROM broker_enrollment_nonces
                 WHERE nonce_sha256 = ?1
                 LIMIT 1",
                (nonce_sha256.to_vec(),),
            )
            .await
            .map_err(|err| LedgerError::Backend(format!("get_enrollment_nonce: {err}")))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|err| LedgerError::Backend(format!("get_enrollment_nonce next: {err}")))?
        else {
            return Ok(None);
        };
        Ok(Some(parse_turso_nonce_row(&row)?))
    }

    async fn claim_enrollment_nonce_and_put_envelope(
        &self,
        nonce_record: &EnrollmentNonceRecord,
        envelope_record: &StoredEnvelopeRecord,
    ) -> Result<NonceClaimOutcome, LedgerError> {
        let envelope_json = serde_json::to_string(envelope_record)
            .map_err(|err| LedgerError::Serialization(err.to_string()))?;

        // Lock ordering: `ledger` -> `vault`. No other method acquires both locks,
        // so this ordering is deadlock-free and ensures concurrent callers cannot
        // observe `Existing` before the winning envelope is durably committed.
        let ledger_guard = self.ledger.lock().await;
        ledger_guard
            .conn
            .execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|err| LedgerError::Backend(format!("BEGIN IMMEDIATE: {err}")))?;

        let insert_res = ledger_guard
            .conn
            .execute(
                "INSERT OR IGNORE INTO broker_enrollment_nonces
                 (nonce_sha256, payload_digest_sha256, tenant_id, credential_id,
                  credential_version, expires_at_ms, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (
                    nonce_record.nonce_sha256.to_vec(),
                    nonce_record.payload_digest_sha256.to_vec(),
                    nonce_record.tenant_id.as_str().to_owned(),
                    nonce_record.credential_id.to_string(),
                    i64::from(nonce_record.credential_version),
                    nonce_record.expires_at_ms,
                    nonce_record.created_at_ms,
                ),
            )
            .await;

        let affected = match insert_res {
            Ok(n) => n,
            Err(err) => {
                let _ = ledger_guard.conn.execute("ROLLBACK", ()).await;
                return Err(LedgerError::Backend(format!(
                    "claim_enrollment_nonce insert: {err}"
                )));
            }
        };

        if affected == 0 {
            let query_res = async {
                let mut rows = ledger_guard
                    .conn
                    .query(
                        "SELECT nonce_sha256, payload_digest_sha256, tenant_id, credential_id,
                                credential_version, expires_at_ms, created_at_ms
                         FROM broker_enrollment_nonces
                         WHERE nonce_sha256 = ?1
                         LIMIT 1",
                        (nonce_record.nonce_sha256.to_vec(),),
                    )
                    .await
                    .map_err(|err| {
                        LedgerError::Backend(format!("claim_enrollment_nonce select: {err}"))
                    })?;
                let row = rows
                    .next()
                    .await
                    .map_err(|err| {
                        LedgerError::Backend(format!("claim_enrollment_nonce next: {err}"))
                    })?
                    .ok_or_else(|| {
                        LedgerError::InvalidState(
                            "enrollment nonce conflict but row missing".to_owned(),
                        )
                    })?;
                parse_turso_nonce_row(&row)
            }
            .await;
            let _ = ledger_guard.conn.execute("ROLLBACK", ()).await;
            return Ok(NonceClaimOutcome::Existing(query_res?));
        }

        // Nonce claimed inside the uncommitted ledger transaction; now commit the
        // vault envelope before committing the ledger so no orphan nonce or
        // premature `Existing` visibility can occur.
        let vault_res: Result<(), LedgerError> = async {
            let vault_guard = self.vault.lock().await;
            let vault = vault_guard
                .as_ref()
                .ok_or_else(|| LedgerError::Backend("vault handle closed".to_owned()))?;
            vault
                .conn
                .execute("BEGIN IMMEDIATE", ())
                .await
                .map_err(|err| LedgerError::Backend(format!("vault BEGIN IMMEDIATE: {err}")))?;
            let write_res = vault
                .conn
                .execute(
                    "INSERT OR REPLACE INTO credential_envelopes
                     (credential_id, tenant_id, account_id, provider, version, record_json, created_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                    (
                        envelope_record.credential_id.to_string(),
                        envelope_record.tenant_id.as_str().to_owned(),
                        envelope_record
                            .account_id
                            .map(|a| a.to_string())
                            .unwrap_or_default(),
                        envelope_record.provider.clone(),
                        i64::from(envelope_record.version),
                        envelope_json,
                        envelope_record.created_at_ms,
                    ),
                )
                .await;
            match write_res {
                Ok(_) => {
                    if let Err(err) = vault.conn.execute("COMMIT", ()).await {
                        let _ = vault.conn.execute("ROLLBACK", ()).await;
                        return Err(LedgerError::Backend(format!("vault COMMIT: {err}")));
                    }
                    Ok(())
                }
                Err(err) => {
                    let _ = vault.conn.execute("ROLLBACK", ()).await;
                    Err(LedgerError::Backend(format!(
                        "claim_enrollment_nonce put_envelope: {err}"
                    )))
                }
            }
        }
        .await;

        if let Err(err) = vault_res {
            let _ = ledger_guard.conn.execute("ROLLBACK", ()).await;
            return Err(err);
        }

        if let Err(err) = ledger_guard.conn.execute("COMMIT", ()).await {
            let _ = ledger_guard.conn.execute("ROLLBACK", ()).await;
            return Err(LedgerError::Backend(format!("ledger COMMIT: {err}")));
        }

        Ok(NonceClaimOutcome::Claimed)
    }

    async fn get_egress_state(&self) -> Result<EgressGateState, LedgerError> {
        let guard = self.ledger.lock().await;
        let mut rows = guard
            .conn
            .query(
                "SELECT egress_open, last_restore_applied_ms
                 FROM broker_egress_state WHERE singleton_id = 1",
                (),
            )
            .await
            .map_err(|err| LedgerError::Backend(format!("get_egress_state: {err}")))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|err| LedgerError::Backend(format!("get_egress_state next: {err}")))?
        else {
            return Ok(EgressGateState {
                egress_open: false,
                last_restore_applied_ms: None,
            });
        };
        Ok(EgressGateState {
            egress_open: col_i64(&row, 0)? == 1,
            last_restore_applied_ms: col_opt_i64(&row, 1)?,
        })
    }

    async fn set_egress_state(&self, state: EgressGateState) -> Result<(), LedgerError> {
        let guard = self.ledger.lock().await;
        guard
            .conn
            .execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|err| LedgerError::Backend(format!("BEGIN IMMEDIATE: {err}")))?;
        let open_int: i64 = if state.egress_open { 1 } else { 0 };
        let res = match state.last_restore_applied_ms {
            Some(ts) => {
                guard
                    .conn
                    .execute(
                        "UPDATE broker_egress_state
                         SET egress_open = ?1, last_restore_applied_ms = ?2
                         WHERE singleton_id = 1",
                        (open_int, ts),
                    )
                    .await
            }
            None => {
                guard
                    .conn
                    .execute(
                        "UPDATE broker_egress_state
                         SET egress_open = ?1
                         WHERE singleton_id = 1",
                        (open_int,),
                    )
                    .await
            }
        };
        match res {
            Ok(_) => {
                guard
                    .conn
                    .execute("COMMIT", ())
                    .await
                    .map_err(|err| LedgerError::Backend(format!("COMMIT: {err}")))?;
                Ok(())
            }
            Err(err) => {
                let _ = guard.conn.execute("ROLLBACK", ()).await;
                Err(LedgerError::Backend(format!("set_egress_state: {err}")))
            }
        }
    }

    async fn put_envelope(&self, record: &StoredEnvelopeRecord) -> Result<(), LedgerError> {
        let json = serde_json::to_string(record)
            .map_err(|err| LedgerError::Serialization(err.to_string()))?;
        let guard = self.vault.lock().await;
        let vault = guard
            .as_ref()
            .ok_or_else(|| LedgerError::Backend("vault handle closed".to_owned()))?;
        vault
            .conn
            .execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|err| LedgerError::Backend(format!("BEGIN IMMEDIATE: {err}")))?;
        let res = vault
            .conn
            .execute(
                "INSERT OR REPLACE INTO credential_envelopes
                 (credential_id, tenant_id, account_id, provider, version, record_json, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
                (
                    record.credential_id.to_string(),
                    record.tenant_id.as_str().to_owned(),
                    record
                        .account_id
                        .map(|a| a.to_string())
                        .unwrap_or_default(),
                    record.provider.clone(),
                    i64::from(record.version),
                    json,
                    record.created_at_ms,
                ),
            )
            .await;
        match res {
            Ok(_) => {
                vault
                    .conn
                    .execute("COMMIT", ())
                    .await
                    .map_err(|err| LedgerError::Backend(format!("COMMIT: {err}")))?;
                Ok(())
            }
            Err(err) => {
                let _ = vault.conn.execute("ROLLBACK", ()).await;
                Err(LedgerError::Backend(format!("put_envelope: {err}")))
            }
        }
    }

    async fn get_envelope(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
    ) -> Result<Option<StoredEnvelopeRecord>, LedgerError> {
        let guard = self.vault.lock().await;
        let vault = guard
            .as_ref()
            .ok_or_else(|| LedgerError::Backend("vault handle closed".to_owned()))?;
        let mut rows = vault
            .conn
            .query(
                "SELECT record_json FROM credential_envelopes
                 WHERE tenant_id = ?1 AND credential_id = ?2
                 LIMIT 1",
                (tenant_id.as_str().to_owned(), credential_id.to_string()),
            )
            .await
            .map_err(|err| LedgerError::Backend(format!("get_envelope: {err}")))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|err| LedgerError::Backend(format!("get_envelope next: {err}")))?
        else {
            return Ok(None);
        };
        let json = col_text(&row, 0)?;
        let record: StoredEnvelopeRecord = serde_json::from_str(&json)
            .map_err(|err| LedgerError::Serialization(err.to_string()))?;
        Ok(Some(record))
    }

    async fn list_envelopes_for_tenant(
        &self,
        tenant_id: &TenantId,
    ) -> Result<Vec<StoredEnvelopeRecord>, LedgerError> {
        let guard = self.vault.lock().await;
        let vault = guard
            .as_ref()
            .ok_or_else(|| LedgerError::Backend("vault handle closed".to_owned()))?;
        let mut rows = vault
            .conn
            .query(
                "SELECT record_json FROM credential_envelopes WHERE tenant_id = ?1",
                (tenant_id.as_str().to_owned(),),
            )
            .await
            .map_err(|err| LedgerError::Backend(format!("list_envelopes_for_tenant: {err}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|err| LedgerError::Backend(format!("list_envelopes next: {err}")))?
        {
            let json = col_text(&row, 0)?;
            let record: StoredEnvelopeRecord = serde_json::from_str(&json)
                .map_err(|err| LedgerError::Serialization(err.to_string()))?;
            out.push(record);
        }
        Ok(out)
    }

    async fn list_all_envelopes(&self) -> Result<Vec<StoredEnvelopeRecord>, LedgerError> {
        let guard = self.vault.lock().await;
        let vault = guard
            .as_ref()
            .ok_or_else(|| LedgerError::Backend("vault handle closed".to_owned()))?;
        let mut rows = vault
            .conn
            .query("SELECT record_json FROM credential_envelopes", ())
            .await
            .map_err(|err| LedgerError::Backend(format!("list_all_envelopes: {err}")))?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|err| LedgerError::Backend(format!("list_all_envelopes next: {err}")))?
        {
            let json = col_text(&row, 0)?;
            let record: StoredEnvelopeRecord = serde_json::from_str(&json)
                .map_err(|err| LedgerError::Serialization(err.to_string()))?;
            out.push(record);
        }
        Ok(out)
    }

    async fn delete_envelope(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
    ) -> Result<bool, LedgerError> {
        let guard = self.vault.lock().await;
        let vault = guard
            .as_ref()
            .ok_or_else(|| LedgerError::Backend("vault handle closed".to_owned()))?;
        let affected = vault
            .conn
            .execute(
                "DELETE FROM credential_envelopes WHERE tenant_id = ?1 AND credential_id = ?2",
                (tenant_id.as_str().to_owned(), credential_id.to_string()),
            )
            .await
            .map_err(|err| LedgerError::Backend(format!("delete_envelope: {err}")))?;
        Ok(affected > 0)
    }

    async fn delete_envelopes_for_account(
        &self,
        tenant_id: &TenantId,
        account_id: AccountId,
    ) -> Result<u64, LedgerError> {
        let guard = self.vault.lock().await;
        let vault = guard
            .as_ref()
            .ok_or_else(|| LedgerError::Backend("vault handle closed".to_owned()))?;
        let affected = vault
            .conn
            .execute(
                "DELETE FROM credential_envelopes WHERE tenant_id = ?1 AND account_id = ?2",
                (tenant_id.as_str().to_owned(), account_id.to_string()),
            )
            .await
            .map_err(|err| LedgerError::Backend(format!("delete_envelopes_for_account: {err}")))?;
        Ok(affected)
    }

    async fn delete_envelopes_for_tenant(&self, tenant_id: &TenantId) -> Result<u64, LedgerError> {
        let guard = self.vault.lock().await;
        let vault = guard
            .as_ref()
            .ok_or_else(|| LedgerError::Backend("vault handle closed".to_owned()))?;
        let affected = vault
            .conn
            .execute(
                "DELETE FROM credential_envelopes WHERE tenant_id = ?1",
                (tenant_id.as_str().to_owned(),),
            )
            .await
            .map_err(|err| LedgerError::Backend(format!("delete_envelopes_for_tenant: {err}")))?;
        Ok(affected)
    }
}

fn parse_turso_nonce_row(row: &turso::Row) -> Result<EnrollmentNonceRecord, LedgerError> {
    let nonce_sha256 = col_blob32(row, 0)?;
    let payload_digest_sha256 = col_blob32(row, 1)?;
    let tenant_id = TenantId::parse(&col_text(row, 2)?)
        .map_err(|err| LedgerError::InvalidState(err.to_string()))?;
    let credential_id = Uuid::parse_str(&col_text(row, 3)?)
        .map_err(|err| LedgerError::InvalidState(err.to_string()))?;
    let credential_version = u32::try_from(col_i64(row, 4)?)
        .map_err(|_| LedgerError::InvalidState("invalid credential_version".to_owned()))?;
    Ok(EnrollmentNonceRecord {
        nonce_sha256,
        payload_digest_sha256,
        tenant_id,
        credential_id,
        credential_version,
        expires_at_ms: col_i64(row, 5)?,
        created_at_ms: col_i64(row, 6)?,
    })
}
