//! PostgreSQL (`sqlx::PgPool`) and SQLite-compat in-process contract
//! implementations of [`DestructionLedgerStore`].

use std::borrow::Cow;

use async_trait::async_trait;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{
    ColumnIndex, Database, Decode, Encode, Executor, IntoArguments, PgPool, Pool, Postgres, Row,
    Sqlite, SqlitePool, Type,
};
use uuid::Uuid;
use waddle_broker_core::identity::{AccountId, TenantId};

use crate::error::LedgerError;
use crate::ledger::{
    DestructionLedgerStore, DestructionReason, DestructionRecord, DestructionTargetKind,
    EgressGateState, EnrollmentNonceRecord, NonceClaimOutcome, RetiredAccountRecord,
    StoredEnvelopeRecord,
};

fn build_ledger_schema(
    int_type: &str,
    blob_type: &str,
    include_retired_jid_index: bool,
    egress_table_ddl: &str,
    egress_seed_ddl: &str,
) -> Vec<String> {
    let mut statements = vec![
        format!(
            "CREATE TABLE IF NOT EXISTS broker_destruction_ledger (
                entry_id TEXT PRIMARY KEY,
                target_kind TEXT NOT NULL CHECK (target_kind IN ('credential', 'credential_version', 'account', 'tenant', 'kms_key')),
                tenant_id TEXT NOT NULL,
                target_id TEXT NOT NULL,
                reason TEXT NOT NULL,
                destroyed_at_ms {int_type} NOT NULL,
                UNIQUE (target_kind, tenant_id, target_id)
            )"
        ),
        format!(
            "CREATE TABLE IF NOT EXISTS broker_retired_accounts (
                tenant_id TEXT NOT NULL,
                account_id TEXT NOT NULL,
                jid_key TEXT NOT NULL,
                localpart_key TEXT NOT NULL,
                retired_at_ms {int_type} NOT NULL,
                PRIMARY KEY (tenant_id, account_id)
            )"
        ),
    ];
    if include_retired_jid_index {
        statements.push(
            "CREATE INDEX IF NOT EXISTS idx_broker_retired_accounts_jid
                ON broker_retired_accounts (tenant_id, jid_key)"
                .to_owned(),
        );
    }
    statements.extend([
        format!(
            "CREATE TABLE IF NOT EXISTS broker_enrollment_nonces (
                nonce_sha256 {blob_type} PRIMARY KEY,
                payload_digest_sha256 {blob_type} NOT NULL,
                tenant_id TEXT NOT NULL,
                credential_id TEXT NOT NULL,
                credential_version INTEGER NOT NULL,
                expires_at_ms {int_type} NOT NULL,
                created_at_ms {int_type} NOT NULL
            )"
        ),
        egress_table_ddl.to_owned(),
        egress_seed_ddl.to_owned(),
        format!(
            "CREATE TABLE IF NOT EXISTS credential_envelopes (
                credential_id TEXT PRIMARY KEY,
                tenant_id TEXT NOT NULL,
                account_id TEXT NOT NULL DEFAULT '',
                provider TEXT NOT NULL,
                version INTEGER NOT NULL,
                record_json TEXT NOT NULL,
                created_at_ms {int_type} NOT NULL
            )"
        ),
    ]);
    statements
}

const PG_EGRESS_TABLE: &str = "CREATE TABLE IF NOT EXISTS broker_egress_state (
    singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    egress_open BOOLEAN NOT NULL,
    last_restore_applied_ms BIGINT
)";

const PG_EGRESS_SEED: &str =
    "INSERT INTO broker_egress_state (singleton_id, egress_open, last_restore_applied_ms)
     VALUES (1, TRUE, NULL) ON CONFLICT (singleton_id) DO NOTHING";

const SQLITE_EGRESS_TABLE: &str = "CREATE TABLE IF NOT EXISTS broker_egress_state (
    singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
    egress_open INTEGER NOT NULL CHECK (egress_open IN (0, 1)),
    last_restore_applied_ms INTEGER
)";

const SQLITE_EGRESS_SEED: &str =
    "INSERT OR IGNORE INTO broker_egress_state (singleton_id, egress_open, last_restore_applied_ms)
     VALUES (1, 1, NULL)";

const SQL_SELECT_DESTRUCTIONS: &str = "SELECT entry_id, target_kind, tenant_id, target_id, reason, destroyed_at_ms FROM broker_destruction_ledger ORDER BY destroyed_at_ms ASC";
const SQL_IS_DESTROYED: &str = "SELECT 1 FROM broker_destruction_ledger WHERE target_kind = $1 AND tenant_id = $2 AND target_id = $3 LIMIT 1";
const SQL_IS_ACCOUNT_RETIRED: &str =
    "SELECT 1 FROM broker_retired_accounts WHERE tenant_id = $1 AND account_id = $2 LIMIT 1";
const SQL_IS_JID_RETIRED: &str =
    "SELECT 1 FROM broker_retired_accounts WHERE tenant_id = $1 AND jid_key = $2 LIMIT 1";
const SQL_SELECT_RETIRED_ACCOUNTS: &str =
    "SELECT tenant_id, account_id, jid_key, localpart_key, retired_at_ms FROM broker_retired_accounts";
const SQL_SELECT_ENROLLMENT_NONCE: &str = "SELECT nonce_sha256, payload_digest_sha256, tenant_id, credential_id, credential_version, expires_at_ms, created_at_ms FROM broker_enrollment_nonces WHERE nonce_sha256 = $1 LIMIT 1";
const SQL_SELECT_EGRESS_STATE: &str =
    "SELECT egress_open, last_restore_applied_ms FROM broker_egress_state WHERE singleton_id = 1";
const SQL_UPDATE_EGRESS_STATE: &str = "UPDATE broker_egress_state SET egress_open = $1, last_restore_applied_ms = COALESCE($2, last_restore_applied_ms) WHERE singleton_id = 1";
const SQL_SELECT_ENVELOPE: &str =
    "SELECT record_json FROM credential_envelopes WHERE tenant_id = $1 AND credential_id = $2 LIMIT 1";
const SQL_SELECT_ENVELOPES_FOR_TENANT: &str =
    "SELECT record_json FROM credential_envelopes WHERE tenant_id = $1";
const SQL_SELECT_ALL_ENVELOPES: &str = "SELECT record_json FROM credential_envelopes";
const SQL_DELETE_ENVELOPE: &str =
    "DELETE FROM credential_envelopes WHERE tenant_id = $1 AND credential_id = $2";
const SQL_DELETE_ENVELOPES_FOR_ACCOUNT: &str =
    "DELETE FROM credential_envelopes WHERE tenant_id = $1 AND account_id = $2";
const SQL_DELETE_ENVELOPES_FOR_TENANT: &str =
    "DELETE FROM credential_envelopes WHERE tenant_id = $1";

fn pg_sql_to_sqlite(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut chars = sql.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '$' && chars.peek().is_some_and(char::is_ascii_digit) {
            out.push('?');
            while chars.peek().is_some_and(char::is_ascii_digit) {
                let _ = chars.next();
            }
        } else {
            out.push(ch);
        }
    }
    out
}

trait LedgerDialect: Database
where
    Self: Sized,
    for<'c> &'c mut Self::Connection: Executor<'c, Database = Self>,
    for<'q> Self::Arguments<'q>: IntoArguments<'q, Self>,
    usize: ColumnIndex<Self::Row>,
    i64: Type<Self> + for<'e> Encode<'e, Self> + for<'r> Decode<'r, Self>,
    Option<i64>: Type<Self> + for<'e> Encode<'e, Self> + for<'r> Decode<'r, Self>,
    String: Type<Self> + for<'e> Encode<'e, Self> + for<'r> Decode<'r, Self>,
    Vec<u8>: Type<Self> + for<'r> Decode<'r, Self>,
    for<'a> &'a str: Type<Self> + Encode<'a, Self>,
    for<'a> &'a [u8]: Type<Self> + Encode<'a, Self>,
{
    type BoolCol: Copy
        + Send
        + Sync
        + Type<Self>
        + for<'e> Encode<'e, Self>
        + for<'r> Decode<'r, Self>;
    type VersionCol: Copy
        + Send
        + Sync
        + Type<Self>
        + for<'e> Encode<'e, Self>
        + for<'r> Decode<'r, Self>;

    const INSERT_DESTRUCTION_SQL: &'static str;
    const UPSERT_RETIRED_ACCOUNT_SQL: &'static str;
    const UPSERT_ENVELOPE_SQL: &'static str;

    fn adapt_sql(pg_sql: &'static str) -> Cow<'static, str>;
    fn encode_bool(value: bool) -> Self::BoolCol;
    fn decode_bool(value: Self::BoolCol) -> bool;
    fn encode_version(version: u32) -> Result<Self::VersionCol, LedgerError>;
    fn decode_version(value: Self::VersionCol) -> Result<u32, LedgerError>;
    fn rows_affected(result: &Self::QueryResult) -> u64;
}

impl LedgerDialect for Postgres {
    type BoolCol = bool;
    type VersionCol = i32;
    const INSERT_DESTRUCTION_SQL: &'static str = "INSERT INTO broker_destruction_ledger
        (entry_id, target_kind, tenant_id, target_id, reason, destroyed_at_ms)
        VALUES ($1, $2, $3, $4, $5, $6)
        ON CONFLICT (target_kind, tenant_id, target_id) DO NOTHING";
    const UPSERT_RETIRED_ACCOUNT_SQL: &'static str = "INSERT INTO broker_retired_accounts
        (tenant_id, account_id, jid_key, localpart_key, retired_at_ms)
        VALUES ($1, $2, $3, $4, $5)
        ON CONFLICT (tenant_id, account_id) DO UPDATE SET
          jid_key = EXCLUDED.jid_key,
          localpart_key = EXCLUDED.localpart_key,
          retired_at_ms = EXCLUDED.retired_at_ms";
    const UPSERT_ENVELOPE_SQL: &'static str = "INSERT INTO credential_envelopes
        (credential_id, tenant_id, account_id, provider, version, record_json, created_at_ms)
        VALUES ($1, $2, $3, $4, $5, $6, $7)
        ON CONFLICT (credential_id) DO UPDATE SET
          version = EXCLUDED.version,
          record_json = EXCLUDED.record_json,
          created_at_ms = EXCLUDED.created_at_ms";

    fn adapt_sql(pg_sql: &'static str) -> Cow<'static, str> {
        Cow::Borrowed(pg_sql)
    }

    fn encode_bool(value: bool) -> Self::BoolCol {
        value
    }

    fn decode_bool(value: Self::BoolCol) -> bool {
        value
    }

    fn encode_version(version: u32) -> Result<Self::VersionCol, LedgerError> {
        i32::try_from(version).map_err(|_| LedgerError::InvalidState("version overflow".to_owned()))
    }

    fn decode_version(value: Self::VersionCol) -> Result<u32, LedgerError> {
        u32::try_from(value).map_err(|_| LedgerError::InvalidState("invalid version".to_owned()))
    }

    fn rows_affected(result: &Self::QueryResult) -> u64 {
        result.rows_affected()
    }
}

impl LedgerDialect for Sqlite {
    type BoolCol = i64;
    type VersionCol = i64;
    const INSERT_DESTRUCTION_SQL: &'static str = "INSERT OR IGNORE INTO broker_destruction_ledger
        (entry_id, target_kind, tenant_id, target_id, reason, destroyed_at_ms)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6)";
    const UPSERT_RETIRED_ACCOUNT_SQL: &'static str =
        "INSERT OR REPLACE INTO broker_retired_accounts
        (tenant_id, account_id, jid_key, localpart_key, retired_at_ms)
        VALUES (?1, ?2, ?3, ?4, ?5)";
    const UPSERT_ENVELOPE_SQL: &'static str = "INSERT OR REPLACE INTO credential_envelopes
        (credential_id, tenant_id, account_id, provider, version, record_json, created_at_ms)
        VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)";

    fn adapt_sql(pg_sql: &'static str) -> Cow<'static, str> {
        Cow::Owned(pg_sql_to_sqlite(pg_sql))
    }

    fn encode_bool(value: bool) -> Self::BoolCol {
        i64::from(value)
    }

    fn decode_bool(value: Self::BoolCol) -> bool {
        value == 1
    }

    fn encode_version(version: u32) -> Result<Self::VersionCol, LedgerError> {
        Ok(i64::from(version))
    }

    fn decode_version(value: Self::VersionCol) -> Result<u32, LedgerError> {
        u32::try_from(value).map_err(|_| LedgerError::InvalidState("invalid version".to_owned()))
    }

    fn rows_affected(result: &Self::QueryResult) -> u64 {
        result.rows_affected()
    }
}

fn backend_err(err: impl std::fmt::Display) -> LedgerError {
    LedgerError::Backend(err.to_string())
}

fn col<'r, D, T>(row: &'r D::Row, index: usize) -> Result<T, LedgerError>
where
    D: Database,
    usize: ColumnIndex<D::Row>,
    T: Decode<'r, D> + Type<D>,
{
    row.try_get(index).map_err(backend_err)
}

struct SqlLedgerStore<D: Database> {
    pool: Pool<D>,
}

impl<D> SqlLedgerStore<D>
where
    D: LedgerDialect,
    for<'c> &'c mut D::Connection: Executor<'c, Database = D>,
    for<'q> D::Arguments<'q>: IntoArguments<'q, D>,
    usize: ColumnIndex<D::Row>,
    i64: Type<D> + for<'e> Encode<'e, D> + for<'r> Decode<'r, D>,
    Option<i64>: Type<D> + for<'e> Encode<'e, D> + for<'r> Decode<'r, D>,
    String: Type<D> + for<'e> Encode<'e, D> + for<'r> Decode<'r, D>,
    Vec<u8>: Type<D> + for<'r> Decode<'r, D>,
    for<'a> &'a str: Type<D> + Encode<'a, D>,
    for<'a> &'a [u8]: Type<D> + Encode<'a, D>,
{
    fn new(pool: Pool<D>) -> Self {
        Self { pool }
    }

    fn decode_nonce_row(row: &D::Row) -> Result<EnrollmentNonceRecord, LedgerError> {
        let n_bytes: Vec<u8> = col::<D, Vec<u8>>(row, 0)?;
        let d_bytes: Vec<u8> = col::<D, Vec<u8>>(row, 1)?;
        let tenant_str: String = col::<D, String>(row, 2)?;
        let cred_str: String = col::<D, String>(row, 3)?;
        let version_col: D::VersionCol = col::<D, D::VersionCol>(row, 4)?;
        Ok(EnrollmentNonceRecord {
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
            credential_version: D::decode_version(version_col)?,
            expires_at_ms: col::<D, i64>(row, 5)?,
            created_at_ms: col::<D, i64>(row, 6)?,
        })
    }

    fn decode_envelope_rows(rows: &[D::Row]) -> Result<Vec<StoredEnvelopeRecord>, LedgerError> {
        rows.iter()
            .map(|row| {
                let json: String = col::<D, String>(row, 0)?;
                serde_json::from_str(&json)
                    .map_err(|err| LedgerError::Serialization(err.to_string()))
            })
            .collect()
    }

    async fn upsert_envelope_with<'e, E>(
        executor: E,
        record: &StoredEnvelopeRecord,
        json: String,
    ) -> Result<(), LedgerError>
    where
        E: Executor<'e, Database = D>,
    {
        let version = D::encode_version(record.version)?;
        let cred_id = record.credential_id.to_string();
        let account_id = record.account_id.map(|a| a.to_string()).unwrap_or_default();
        sqlx::query::<D>(D::UPSERT_ENVELOPE_SQL)
            .bind(cred_id.as_str())
            .bind(record.tenant_id.as_str())
            .bind(account_id.as_str())
            .bind(record.provider.as_str())
            .bind(version)
            .bind(json)
            .bind(record.created_at_ms)
            .execute(executor)
            .await
            .map_err(backend_err)?;
        Ok(())
    }

    async fn fetch_existing_nonce_with<'e, E>(
        executor: E,
        nonce_sha256: &[u8; 32],
    ) -> Result<EnrollmentNonceRecord, LedgerError>
    where
        E: Executor<'e, Database = D>,
    {
        let sql = D::adapt_sql(SQL_SELECT_ENROLLMENT_NONCE);
        let row = sqlx::query::<D>(&sql)
            .bind(nonce_sha256.as_slice())
            .fetch_optional(executor)
            .await
            .map_err(backend_err)?
            .ok_or_else(|| {
                LedgerError::InvalidState("enrollment nonce conflict but row missing".to_owned())
            })?;
        Self::decode_nonce_row(&row)
    }

    async fn append_destruction(&self, record: &DestructionRecord) -> Result<(), LedgerError> {
        let entry_id = record.entry_id.to_string();
        sqlx::query::<D>(D::INSERT_DESTRUCTION_SQL)
            .bind(entry_id.as_str())
            .bind(record.target_kind.as_str())
            .bind(record.tenant_id.as_str())
            .bind(record.target_id.as_str())
            .bind(record.reason.as_str())
            .bind(record.destroyed_at_ms)
            .execute(&self.pool)
            .await
            .map_err(backend_err)?;
        Ok(())
    }

    async fn list_destructions(&self) -> Result<Vec<DestructionRecord>, LedgerError> {
        let rows = sqlx::query::<D>(SQL_SELECT_DESTRUCTIONS)
            .fetch_all(&self.pool)
            .await
            .map_err(backend_err)?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let entry_id_str: String = col::<D, String>(&row, 0)?;
            let kind_str: String = col::<D, String>(&row, 1)?;
            let tenant_str: String = col::<D, String>(&row, 2)?;
            let target_id: String = col::<D, String>(&row, 3)?;
            let reason_str: String = col::<D, String>(&row, 4)?;
            let destroyed_at_ms: i64 = col::<D, i64>(&row, 5)?;
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
        let sql = D::adapt_sql(SQL_IS_DESTROYED);
        let found = sqlx::query::<D>(&sql)
            .bind(kind.as_str())
            .bind(tenant_id.as_str())
            .bind(target_id)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend_err)?;
        Ok(found.is_some())
    }

    async fn record_retired_account(
        &self,
        record: &RetiredAccountRecord,
    ) -> Result<(), LedgerError> {
        let account_id = record.account_id.to_string();
        sqlx::query::<D>(D::UPSERT_RETIRED_ACCOUNT_SQL)
            .bind(record.tenant_id.as_str())
            .bind(account_id.as_str())
            .bind(record.jid_key.as_str())
            .bind(record.localpart_key.as_str())
            .bind(record.retired_at_ms)
            .execute(&self.pool)
            .await
            .map_err(backend_err)?;
        Ok(())
    }

    async fn is_account_retired(
        &self,
        tenant_id: &TenantId,
        account_id: AccountId,
    ) -> Result<bool, LedgerError> {
        let account_str = account_id.to_string();
        let sql = D::adapt_sql(SQL_IS_ACCOUNT_RETIRED);
        let found = sqlx::query::<D>(&sql)
            .bind(tenant_id.as_str())
            .bind(account_str.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend_err)?;
        Ok(found.is_some())
    }

    async fn is_jid_retired(
        &self,
        tenant_id: &TenantId,
        jid_key: &str,
    ) -> Result<bool, LedgerError> {
        let sql = D::adapt_sql(SQL_IS_JID_RETIRED);
        let found = sqlx::query::<D>(&sql)
            .bind(tenant_id.as_str())
            .bind(jid_key)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend_err)?;
        Ok(found.is_some())
    }

    async fn list_retired_accounts(&self) -> Result<Vec<RetiredAccountRecord>, LedgerError> {
        let rows = sqlx::query::<D>(SQL_SELECT_RETIRED_ACCOUNTS)
            .fetch_all(&self.pool)
            .await
            .map_err(backend_err)?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let tenant_str: String = col::<D, String>(&row, 0)?;
            let account_str: String = col::<D, String>(&row, 1)?;
            out.push(RetiredAccountRecord {
                tenant_id: TenantId::parse(&tenant_str)
                    .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
                account_id: AccountId::from_uuid(
                    Uuid::parse_str(&account_str)
                        .map_err(|err| LedgerError::InvalidState(err.to_string()))?,
                ),
                jid_key: col::<D, String>(&row, 2)?,
                localpart_key: col::<D, String>(&row, 3)?,
                retired_at_ms: col::<D, i64>(&row, 4)?,
            });
        }
        Ok(out)
    }

    async fn get_enrollment_nonce(
        &self,
        nonce_sha256: &[u8; 32],
    ) -> Result<Option<EnrollmentNonceRecord>, LedgerError> {
        let sql = D::adapt_sql(SQL_SELECT_ENROLLMENT_NONCE);
        let row = sqlx::query::<D>(&sql)
            .bind(nonce_sha256.as_slice())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend_err)?;
        row.as_ref().map(Self::decode_nonce_row).transpose()
    }

    async fn get_egress_state(&self) -> Result<EgressGateState, LedgerError> {
        let Some(row) = sqlx::query::<D>(SQL_SELECT_EGRESS_STATE)
            .fetch_optional(&self.pool)
            .await
            .map_err(backend_err)?
        else {
            return Ok(EgressGateState {
                egress_open: false,
                last_restore_applied_ms: None,
            });
        };
        let open_raw: D::BoolCol = col::<D, D::BoolCol>(&row, 0)?;
        Ok(EgressGateState {
            egress_open: D::decode_bool(open_raw),
            last_restore_applied_ms: col::<D, Option<i64>>(&row, 1)?,
        })
    }

    async fn set_egress_state(&self, state: EgressGateState) -> Result<(), LedgerError> {
        let sql = D::adapt_sql(SQL_UPDATE_EGRESS_STATE);
        sqlx::query::<D>(&sql)
            .bind(D::encode_bool(state.egress_open))
            .bind(state.last_restore_applied_ms)
            .execute(&self.pool)
            .await
            .map_err(backend_err)?;
        Ok(())
    }

    async fn put_envelope(&self, record: &StoredEnvelopeRecord) -> Result<(), LedgerError> {
        let json = serde_json::to_string(record)
            .map_err(|err| LedgerError::Serialization(err.to_string()))?;
        Self::upsert_envelope_with(&self.pool, record, json).await
    }

    async fn get_envelope(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
    ) -> Result<Option<StoredEnvelopeRecord>, LedgerError> {
        let cred_str = credential_id.to_string();
        let sql = D::adapt_sql(SQL_SELECT_ENVELOPE);
        let Some(row) = sqlx::query::<D>(&sql)
            .bind(tenant_id.as_str())
            .bind(cred_str.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(backend_err)?
        else {
            return Ok(None);
        };
        let json: String = col::<D, String>(&row, 0)?;
        let record: StoredEnvelopeRecord = serde_json::from_str(&json)
            .map_err(|err| LedgerError::Serialization(err.to_string()))?;
        Ok(Some(record))
    }

    async fn list_envelopes_for_tenant(
        &self,
        tenant_id: &TenantId,
    ) -> Result<Vec<StoredEnvelopeRecord>, LedgerError> {
        let sql = D::adapt_sql(SQL_SELECT_ENVELOPES_FOR_TENANT);
        let rows = sqlx::query::<D>(&sql)
            .bind(tenant_id.as_str())
            .fetch_all(&self.pool)
            .await
            .map_err(backend_err)?;
        Self::decode_envelope_rows(&rows)
    }

    async fn list_all_envelopes(&self) -> Result<Vec<StoredEnvelopeRecord>, LedgerError> {
        let rows = sqlx::query::<D>(SQL_SELECT_ALL_ENVELOPES)
            .fetch_all(&self.pool)
            .await
            .map_err(backend_err)?;
        Self::decode_envelope_rows(&rows)
    }

    async fn delete_envelope(
        &self,
        tenant_id: &TenantId,
        credential_id: Uuid,
    ) -> Result<bool, LedgerError> {
        let cred_str = credential_id.to_string();
        let sql = D::adapt_sql(SQL_DELETE_ENVELOPE);
        let res = sqlx::query::<D>(&sql)
            .bind(tenant_id.as_str())
            .bind(cred_str.as_str())
            .execute(&self.pool)
            .await
            .map_err(backend_err)?;
        Ok(D::rows_affected(&res) > 0)
    }

    async fn delete_envelopes_for_account(
        &self,
        tenant_id: &TenantId,
        account_id: AccountId,
    ) -> Result<u64, LedgerError> {
        let account_str = account_id.to_string();
        let sql = D::adapt_sql(SQL_DELETE_ENVELOPES_FOR_ACCOUNT);
        let res = sqlx::query::<D>(&sql)
            .bind(tenant_id.as_str())
            .bind(account_str.as_str())
            .execute(&self.pool)
            .await
            .map_err(backend_err)?;
        Ok(D::rows_affected(&res))
    }

    async fn delete_envelopes_for_tenant(&self, tenant_id: &TenantId) -> Result<u64, LedgerError> {
        let sql = D::adapt_sql(SQL_DELETE_ENVELOPES_FOR_TENANT);
        let res = sqlx::query::<D>(&sql)
            .bind(tenant_id.as_str())
            .execute(&self.pool)
            .await
            .map_err(backend_err)?;
        Ok(D::rows_affected(&res))
    }
}

macro_rules! impl_destruction_ledger_store {
    ($store:ty, $claim_fn:ident) => {
        #[async_trait]
        impl DestructionLedgerStore for $store {
            async fn append_destruction(
                &self,
                record: &DestructionRecord,
            ) -> Result<(), LedgerError> {
                self.inner.append_destruction(record).await
            }

            async fn list_destructions(&self) -> Result<Vec<DestructionRecord>, LedgerError> {
                self.inner.list_destructions().await
            }

            async fn is_destroyed(
                &self,
                kind: DestructionTargetKind,
                tenant_id: &TenantId,
                target_id: &str,
            ) -> Result<bool, LedgerError> {
                self.inner.is_destroyed(kind, tenant_id, target_id).await
            }

            async fn record_retired_account(
                &self,
                record: &RetiredAccountRecord,
            ) -> Result<(), LedgerError> {
                self.inner.record_retired_account(record).await
            }

            async fn is_account_retired(
                &self,
                tenant_id: &TenantId,
                account_id: AccountId,
            ) -> Result<bool, LedgerError> {
                self.inner.is_account_retired(tenant_id, account_id).await
            }

            async fn is_jid_retired(
                &self,
                tenant_id: &TenantId,
                jid_key: &str,
            ) -> Result<bool, LedgerError> {
                self.inner.is_jid_retired(tenant_id, jid_key).await
            }

            async fn list_retired_accounts(
                &self,
            ) -> Result<Vec<RetiredAccountRecord>, LedgerError> {
                self.inner.list_retired_accounts().await
            }

            async fn get_enrollment_nonce(
                &self,
                nonce_sha256: &[u8; 32],
            ) -> Result<Option<EnrollmentNonceRecord>, LedgerError> {
                self.inner.get_enrollment_nonce(nonce_sha256).await
            }

            async fn claim_enrollment_nonce_and_put_envelope(
                &self,
                nonce_record: &EnrollmentNonceRecord,
                envelope_record: &StoredEnvelopeRecord,
            ) -> Result<NonceClaimOutcome, LedgerError> {
                self.$claim_fn(nonce_record, envelope_record).await
            }

            async fn get_egress_state(&self) -> Result<EgressGateState, LedgerError> {
                self.inner.get_egress_state().await
            }

            async fn set_egress_state(&self, state: EgressGateState) -> Result<(), LedgerError> {
                self.inner.set_egress_state(state).await
            }

            async fn put_envelope(&self, record: &StoredEnvelopeRecord) -> Result<(), LedgerError> {
                self.inner.put_envelope(record).await
            }

            async fn get_envelope(
                &self,
                tenant_id: &TenantId,
                credential_id: Uuid,
            ) -> Result<Option<StoredEnvelopeRecord>, LedgerError> {
                self.inner.get_envelope(tenant_id, credential_id).await
            }

            async fn list_envelopes_for_tenant(
                &self,
                tenant_id: &TenantId,
            ) -> Result<Vec<StoredEnvelopeRecord>, LedgerError> {
                self.inner.list_envelopes_for_tenant(tenant_id).await
            }

            async fn list_all_envelopes(&self) -> Result<Vec<StoredEnvelopeRecord>, LedgerError> {
                self.inner.list_all_envelopes().await
            }

            async fn delete_envelope(
                &self,
                tenant_id: &TenantId,
                credential_id: Uuid,
            ) -> Result<bool, LedgerError> {
                self.inner.delete_envelope(tenant_id, credential_id).await
            }

            async fn delete_envelopes_for_account(
                &self,
                tenant_id: &TenantId,
                account_id: AccountId,
            ) -> Result<u64, LedgerError> {
                self.inner
                    .delete_envelopes_for_account(tenant_id, account_id)
                    .await
            }

            async fn delete_envelopes_for_tenant(
                &self,
                tenant_id: &TenantId,
            ) -> Result<u64, LedgerError> {
                self.inner.delete_envelopes_for_tenant(tenant_id).await
            }
        }
    };
}

/// PostgreSQL [`DestructionLedgerStore`] backed by `sqlx::PgPool`.
pub struct PostgresLedgerStore {
    inner: SqlLedgerStore<Postgres>,
}

impl PostgresLedgerStore {
    pub async fn new(pool: PgPool) -> Result<Self, LedgerError> {
        for stmt in build_ledger_schema("BIGINT", "BYTEA", true, PG_EGRESS_TABLE, PG_EGRESS_SEED) {
            sqlx::query(&stmt)
                .execute(&pool)
                .await
                .map_err(|err| LedgerError::Backend(format!("pg schema: {err}")))?;
        }
        Ok(Self {
            inner: SqlLedgerStore::new(pool),
        })
    }

    async fn claim_nonce_pg(
        &self,
        nonce_record: &EnrollmentNonceRecord,
        envelope_record: &StoredEnvelopeRecord,
    ) -> Result<NonceClaimOutcome, LedgerError> {
        let nonce_version_i32 = Postgres::encode_version(nonce_record.credential_version)?;
        let env_json = serde_json::to_string(envelope_record)
            .map_err(|err| LedgerError::Serialization(err.to_string()))?;

        let mut tx = self.inner.pool.begin().await.map_err(backend_err)?;
        let inserted = sqlx::query(
            "INSERT INTO broker_enrollment_nonces
             (nonce_sha256, payload_digest_sha256, tenant_id, credential_id,
              credential_version, expires_at_ms, created_at_ms)
             VALUES ($1, $2, $3, $4, $5, $6, $7)
             ON CONFLICT (nonce_sha256) DO NOTHING
             RETURNING nonce_sha256",
        )
        .bind(nonce_record.nonce_sha256.as_slice())
        .bind(nonce_record.payload_digest_sha256.as_slice())
        .bind(nonce_record.tenant_id.as_str())
        .bind(nonce_record.credential_id.to_string())
        .bind(nonce_version_i32)
        .bind(nonce_record.expires_at_ms)
        .bind(nonce_record.created_at_ms)
        .fetch_optional(&mut *tx)
        .await
        .map_err(backend_err)?;

        if inserted.is_some() {
            SqlLedgerStore::<Postgres>::upsert_envelope_with(&mut *tx, envelope_record, env_json)
                .await?;
            tx.commit().await.map_err(backend_err)?;
            Ok(NonceClaimOutcome::Claimed)
        } else {
            let existing = SqlLedgerStore::<Postgres>::fetch_existing_nonce_with(
                &mut *tx,
                &nonce_record.nonce_sha256,
            )
            .await?;
            let _ = tx.rollback().await;
            Ok(NonceClaimOutcome::Existing(existing))
        }
    }
}

impl_destruction_ledger_store!(PostgresLedgerStore, claim_nonce_pg);

/// SQLite-compat in-process contract store matching [`PostgresLedgerStore`] SQL semantics.
pub struct SqliteCompatLedgerStore {
    inner: SqlLedgerStore<Sqlite>,
}

impl SqliteCompatLedgerStore {
    pub async fn connect_in_memory() -> Result<Self, LedgerError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .map_err(backend_err)?;
        Self::new(pool).await
    }

    pub async fn new(pool: SqlitePool) -> Result<Self, LedgerError> {
        for stmt in build_ledger_schema(
            "INTEGER",
            "BLOB",
            false,
            SQLITE_EGRESS_TABLE,
            SQLITE_EGRESS_SEED,
        ) {
            sqlx::query(&stmt)
                .execute(&pool)
                .await
                .map_err(backend_err)?;
        }
        Ok(Self {
            inner: SqlLedgerStore::new(pool),
        })
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
            .execute(&self.inner.pool)
            .await
            .map_err(backend_err)?;
        for rec in snapshot {
            self.put_envelope(rec).await?;
        }
        Ok(())
    }

    async fn claim_nonce_sqlite(
        &self,
        nonce_record: &EnrollmentNonceRecord,
        envelope_record: &StoredEnvelopeRecord,
    ) -> Result<NonceClaimOutcome, LedgerError> {
        let env_json = serde_json::to_string(envelope_record)
            .map_err(|err| LedgerError::Serialization(err.to_string()))?;
        let mut conn = self.inner.pool.acquire().await.map_err(backend_err)?;
        sqlx::query("BEGIN IMMEDIATE")
            .execute(&mut *conn)
            .await
            .map_err(backend_err)?;

        let tx_res: Result<NonceClaimOutcome, LedgerError> = async {
            let res = sqlx::query(
                "INSERT OR IGNORE INTO broker_enrollment_nonces
                 (nonce_sha256, payload_digest_sha256, tenant_id, credential_id,
                  credential_version, expires_at_ms, created_at_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            )
            .bind(nonce_record.nonce_sha256.as_slice())
            .bind(nonce_record.payload_digest_sha256.as_slice())
            .bind(nonce_record.tenant_id.as_str())
            .bind(nonce_record.credential_id.to_string())
            .bind(i64::from(nonce_record.credential_version))
            .bind(nonce_record.expires_at_ms)
            .bind(nonce_record.created_at_ms)
            .execute(&mut *conn)
            .await
            .map_err(backend_err)?;

            if res.rows_affected() > 0 {
                SqlLedgerStore::<Sqlite>::upsert_envelope_with(
                    &mut *conn,
                    envelope_record,
                    env_json,
                )
                .await?;
                Ok(NonceClaimOutcome::Claimed)
            } else {
                let existing = SqlLedgerStore::<Sqlite>::fetch_existing_nonce_with(
                    &mut *conn,
                    &nonce_record.nonce_sha256,
                )
                .await?;
                Ok(NonceClaimOutcome::Existing(existing))
            }
        }
        .await;

        match tx_res {
            Ok(NonceClaimOutcome::Claimed) => {
                if let Err(err) = sqlx::query("COMMIT").execute(&mut *conn).await {
                    let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                    return Err(backend_err(err));
                }
                Ok(NonceClaimOutcome::Claimed)
            }
            Ok(NonceClaimOutcome::Existing(existing)) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                Ok(NonceClaimOutcome::Existing(existing))
            }
            Err(err) => {
                let _ = sqlx::query("ROLLBACK").execute(&mut *conn).await;
                Err(err)
            }
        }
    }
}

impl_destruction_ledger_store!(SqliteCompatLedgerStore, claim_nonce_sqlite);
