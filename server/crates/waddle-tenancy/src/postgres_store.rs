//! Multi-tenant PostgreSQL [`TenantStore`] (`PostgresTenantStore` backed by
//! `sqlx::PgPool`) plus an in-process SQLite-backed multi-tenant schema contract
//! adapter (`SqliteCompatPostgresStore` backed by `sqlx::SqlitePool`) so the
//! identical `tenant_id`-scoped schema and composite uniqueness invariants run
//! unconditionally on hosts without a local Postgres daemon.

use std::borrow::Cow;
use std::sync::Arc;

use async_trait::async_trait;
use sqlx::{
    ColumnIndex, Database, Decode, Encode, Executor, IntoArguments, PgPool, Pool, Postgres, Row,
    Sqlite, SqlitePool, Type,
};
use uuid::Uuid;
use waddle_broker_core::identity::{AccountId, TenantId};
use waddle_broker_core::placement::{FenceGeneration, PlacementLeaseStore};
use waddle_broker_core::store::{
    StorageBackendKind, TenantDbHandle, TenantQueryScope, TenantStore, TenantStoreError,
};

use crate::data_ops::{
    RemoteParticipantRecord, TenantAuthIdentityRecord, TenantDataOps, TenantJobRecord,
    TenantSpaceRecord, TenantUserRecord,
};

const PG_HEAD_SCHEMA: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS tenant_registry_tenants (
        tenant_id TEXT PRIMARY KEY,
        created_at TIMESTAMPTZ NOT NULL DEFAULT now()
    )"#,
    r#"CREATE TABLE IF NOT EXISTS tenant_placement_fences (
        tenant_id TEXT PRIMARY KEY REFERENCES tenant_registry_tenants(tenant_id) ON DELETE CASCADE,
        holder_spiffe_id TEXT NOT NULL,
        holder_incarnation_pubkey BYTEA NOT NULL CHECK (octet_length(holder_incarnation_pubkey) = 32),
        generation BIGINT NOT NULL CHECK (generation >= 1),
        updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
    )"#,
];

const SQLITE_HEAD_SCHEMA: &[&str] = &[
    "PRAGMA foreign_keys = ON",
    r#"CREATE TABLE IF NOT EXISTS tenant_registry_tenants (
        tenant_id TEXT PRIMARY KEY
    )"#,
    r#"CREATE TABLE IF NOT EXISTS tenant_placement_fences (
        tenant_id TEXT PRIMARY KEY REFERENCES tenant_registry_tenants(tenant_id) ON DELETE CASCADE,
        holder_spiffe_id TEXT NOT NULL,
        holder_incarnation_pubkey BLOB NOT NULL CHECK (length(holder_incarnation_pubkey) = 32),
        generation INTEGER NOT NULL CHECK (generation >= 1)
    )"#,
];

const SHARED_DATA_SCHEMA: &[&str] = &[
    r#"CREATE TABLE IF NOT EXISTS tenant_users (
        tenant_id TEXT NOT NULL REFERENCES tenant_registry_tenants(tenant_id) ON DELETE CASCADE,
        jid TEXT NOT NULL,
        account_id TEXT NOT NULL,
        username TEXT NOT NULL,
        xmpp_localpart TEXT NOT NULL,
        display_name TEXT,
        PRIMARY KEY (tenant_id, jid),
        UNIQUE (tenant_id, account_id),
        UNIQUE (tenant_id, username),
        UNIQUE (tenant_id, xmpp_localpart)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS tenant_auth_identities (
        tenant_id TEXT NOT NULL REFERENCES tenant_registry_tenants(tenant_id) ON DELETE CASCADE,
        id TEXT NOT NULL,
        user_jid TEXT NOT NULL,
        provider_id TEXT NOT NULL,
        issuer TEXT NOT NULL,
        subject TEXT NOT NULL,
        PRIMARY KEY (tenant_id, id),
        UNIQUE (tenant_id, issuer, subject)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS tenant_spaces_metadata (
        tenant_id TEXT NOT NULL REFERENCES tenant_registry_tenants(tenant_id) ON DELETE CASCADE,
        space_node TEXT NOT NULL,
        space_jid TEXT NOT NULL,
        name TEXT NOT NULL,
        PRIMARY KEY (tenant_id, space_node)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS tenant_remote_participants (
        tenant_id TEXT NOT NULL REFERENCES tenant_registry_tenants(tenant_id) ON DELETE CASCADE,
        room_id TEXT NOT NULL,
        remote_bare_jid TEXT NOT NULL,
        affiliation TEXT NOT NULL,
        PRIMARY KEY (tenant_id, room_id, remote_bare_jid)
    )"#,
    r#"CREATE TABLE IF NOT EXISTS tenant_jobs (
        tenant_id TEXT NOT NULL REFERENCES tenant_registry_tenants(tenant_id) ON DELETE CASCADE,
        job_id TEXT NOT NULL,
        queue TEXT NOT NULL,
        payload_json TEXT NOT NULL,
        status TEXT NOT NULL,
        PRIMARY KEY (tenant_id, job_id)
    )"#,
];

const PG_TAIL_SCHEMA: &[&str] = &[
    r#"CREATE OR REPLACE FUNCTION reject_tenant_user_account_id_change() RETURNS TRIGGER AS $$
       BEGIN
           IF OLD.account_id IS NOT NULL AND NEW.account_id IS DISTINCT FROM OLD.account_id THEN
               RAISE EXCEPTION 'account_id is immutable';
           END IF;
           RETURN NEW;
       END;
       $$ LANGUAGE plpgsql"#,
    r#"DROP TRIGGER IF EXISTS trg_tenant_users_account_id_immutable ON tenant_users"#,
    r#"CREATE TRIGGER trg_tenant_users_account_id_immutable
       BEFORE UPDATE OF account_id ON tenant_users
       FOR EACH ROW EXECUTE FUNCTION reject_tenant_user_account_id_change()"#,
    r#"CREATE TABLE IF NOT EXISTS tenant_extension_room_observers (
        tenant_id TEXT NOT NULL REFERENCES tenant_registry_tenants(tenant_id) ON DELETE CASCADE,
        plugin_id TEXT NOT NULL,
        generation BIGINT NOT NULL,
        identity TEXT NOT NULL,
        scope_json TEXT NOT NULL,
        max_concurrent INTEGER NOT NULL,
        PRIMARY KEY (tenant_id, plugin_id)
    )"#,
];

const SQLITE_TAIL_SCHEMA: &[&str] = &[
    r#"CREATE TRIGGER IF NOT EXISTS trg_tenant_users_account_id_immutable
       BEFORE UPDATE OF account_id ON tenant_users
       WHEN OLD.account_id != NEW.account_id
       BEGIN
           SELECT RAISE(ABORT, 'account_id is immutable');
       END"#,
];

// ============================================================================
// Shared validation, fence checking, row decoding, and SQL backend abstraction
// ============================================================================

async fn verify_lease(
    lease_store: Option<&Arc<dyn PlacementLeaseStore>>,
    scope: &TenantQueryScope,
) -> Result<(), TenantStoreError> {
    let Some(lease_store) = lease_store else {
        return Ok(());
    };
    let current = lease_store
        .current(&scope.tenant_id)
        .await
        .map_err(|err| TenantStoreError::Backend(format!("lease lookup failed: {err}")))?
        .ok_or_else(|| TenantStoreError::UnauthorizedHolder(scope.tenant_id.clone()))?;
    if current.generation != scope.placement_generation {
        return Err(TenantStoreError::StalePlacementGeneration {
            tenant_id: scope.tenant_id.clone(),
            expected: current.generation,
            actual: scope.placement_generation,
        });
    }
    if current.holder.spiffe_id != scope.holder.spiffe_id
        || current.holder.incarnation_pubkey != scope.holder.incarnation_pubkey
    {
        return Err(TenantStoreError::UnauthorizedHolder(
            scope.tenant_id.clone(),
        ));
    }
    Ok(())
}

fn verify_fence_match(
    scope: &TenantQueryScope,
    spiffe: &str,
    pk: &[u8],
    gen_i64: i64,
) -> Result<(), TenantStoreError> {
    let current_gen = FenceGeneration::new(u64::try_from(gen_i64).unwrap_or(0));
    if current_gen != scope.placement_generation {
        return Err(TenantStoreError::StalePlacementGeneration {
            tenant_id: scope.tenant_id.clone(),
            expected: current_gen,
            actual: scope.placement_generation,
        });
    }
    if spiffe != scope.holder.spiffe_id.as_str() || pk != scope.holder.incarnation_pubkey.as_bytes()
    {
        return Err(TenantStoreError::UnauthorizedHolder(
            scope.tenant_id.clone(),
        ));
    }
    Ok(())
}

fn verify_fence_advance(
    scope: &TenantQueryScope,
    spiffe: &str,
    pk: &[u8],
    gen_i64: i64,
) -> Result<(), TenantStoreError> {
    let current_gen = FenceGeneration::new(u64::try_from(gen_i64).unwrap_or(0));
    if scope.placement_generation < current_gen {
        return Err(TenantStoreError::StalePlacementGeneration {
            tenant_id: scope.tenant_id.clone(),
            expected: current_gen,
            actual: scope.placement_generation,
        });
    }
    if scope.placement_generation == current_gen
        && (spiffe != scope.holder.spiffe_id.as_str()
            || pk != scope.holder.incarnation_pubkey.as_bytes())
    {
        return Err(TenantStoreError::UnauthorizedHolder(
            scope.tenant_id.clone(),
        ));
    }
    Ok(())
}

fn decode_user_row<DB>(row: &DB::Row) -> Result<TenantUserRecord, TenantStoreError>
where
    DB: Database,
    usize: ColumnIndex<DB::Row>,
    for<'r> String: Decode<'r, DB> + Type<DB>,
    for<'r> Option<String>: Decode<'r, DB> + Type<DB>,
{
    let account_id_str: String = row.get(1);
    let uuid = Uuid::parse_str(&account_id_str)
        .map_err(|e| TenantStoreError::Backend(format!("invalid account_id uuid: {e}")))?;
    Ok(TenantUserRecord {
        account_id: AccountId::from_uuid(uuid),
        jid: row.get(0),
        username: row.get(2),
        xmpp_localpart: row.get(3),
        display_name: row.get(4),
    })
}

fn decode_auth_identity_row<DB>(row: &DB::Row) -> TenantAuthIdentityRecord
where
    DB: Database,
    usize: ColumnIndex<DB::Row>,
    for<'r> String: Decode<'r, DB> + Type<DB>,
{
    TenantAuthIdentityRecord {
        id: row.get(0),
        user_jid: row.get(1),
        provider_id: row.get(2),
        issuer: row.get(3),
        subject: row.get(4),
    }
}

fn decode_space_row<DB>(row: &DB::Row) -> TenantSpaceRecord
where
    DB: Database,
    usize: ColumnIndex<DB::Row>,
    for<'r> String: Decode<'r, DB> + Type<DB>,
{
    TenantSpaceRecord {
        space_node: row.get(0),
        space_jid: row.get(1),
        name: row.get(2),
    }
}

fn decode_remote_participant_row<DB>(row: &DB::Row) -> RemoteParticipantRecord
where
    DB: Database,
    usize: ColumnIndex<DB::Row>,
    for<'r> String: Decode<'r, DB> + Type<DB>,
{
    RemoteParticipantRecord {
        room_id: row.get(0),
        remote_bare_jid: row.get(1),
        affiliation: row.get(2),
    }
}

#[async_trait]
trait TenantSqlDb: Database {
    fn adapt_sql(pg_sql: &'static str) -> Cow<'static, str>;

    fn rows_affected(result: &Self::QueryResult) -> u64;

    async fn claim_next_job_raw(
        pool: &Pool<Self>,
        tenant_id: &str,
        queue: &str,
    ) -> Result<Option<TenantJobRecord>, TenantStoreError>;
}

#[async_trait]
impl TenantSqlDb for Postgres {
    fn adapt_sql(pg_sql: &'static str) -> Cow<'static, str> {
        Cow::Borrowed(pg_sql)
    }

    fn rows_affected(result: &Self::QueryResult) -> u64 {
        result.rows_affected()
    }

    async fn claim_next_job_raw(
        pool: &Pool<Self>,
        tenant_id: &str,
        queue: &str,
    ) -> Result<Option<TenantJobRecord>, TenantStoreError> {
        let row = sqlx::query(
            "UPDATE tenant_jobs
             SET status = 'claimed'
             WHERE (tenant_id, job_id) IN (
                 SELECT tenant_id, job_id FROM tenant_jobs
                 WHERE tenant_id = $1 AND queue = $2 AND status = 'pending'
                 ORDER BY job_id
                 LIMIT 1
                 FOR UPDATE SKIP LOCKED
             )
             RETURNING job_id, queue, payload_json, status",
        )
        .bind(tenant_id)
        .bind(queue)
        .fetch_optional(pool)
        .await
        .map_err(|err| TenantStoreError::Backend(format!("claim_next_job failed: {err}")))?;
        Ok(row.map(|row| TenantJobRecord {
            job_id: row.get(0),
            queue: row.get(1),
            payload_json: row.get(2),
            status: row.get(3),
        }))
    }
}

#[async_trait]
impl TenantSqlDb for Sqlite {
    fn adapt_sql(pg_sql: &'static str) -> Cow<'static, str> {
        Cow::Owned(
            pg_sql
                .replace(" FOR UPDATE", "")
                .replace(",\n                     updated_at = now()", "")
                .replace('$', "?"),
        )
    }

    fn rows_affected(result: &Self::QueryResult) -> u64 {
        result.rows_affected()
    }

    async fn claim_next_job_raw(
        pool: &Pool<Self>,
        tenant_id: &str,
        queue: &str,
    ) -> Result<Option<TenantJobRecord>, TenantStoreError> {
        loop {
            let row = sqlx::query(
                "SELECT job_id, queue, payload_json, status
                 FROM tenant_jobs
                 WHERE tenant_id = ?1 AND queue = ?2 AND status = 'pending'
                 ORDER BY job_id LIMIT 1",
            )
            .bind(tenant_id)
            .bind(queue)
            .fetch_optional(pool)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("claim_next_job query: {err}")))?;
            let Some(row) = row else {
                return Ok(None);
            };
            let job_id: String = row.get(0);
            let queue_val: String = row.get(1);
            let payload_json: String = row.get(2);
            let result = sqlx::query(
                "UPDATE tenant_jobs SET status = 'claimed'
                 WHERE tenant_id = ?1 AND job_id = ?2 AND status = 'pending'",
            )
            .bind(tenant_id)
            .bind(&job_id)
            .execute(pool)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("claim_next_job update: {err}")))?;
            if result.rows_affected() == 1 {
                return Ok(Some(TenantJobRecord {
                    job_id,
                    queue: queue_val,
                    payload_json,
                    status: "claimed".to_owned(),
                }));
            }
        }
    }
}

struct SqlTenantStoreCore<DB: Database> {
    pool: Pool<DB>,
    lease_store: Option<Arc<dyn PlacementLeaseStore>>,
}

impl<DB> SqlTenantStoreCore<DB>
where
    DB: TenantSqlDb,
    for<'c> &'c Pool<DB>: Executor<'c, Database = DB>,
    for<'c> &'c mut DB::Connection: Executor<'c, Database = DB>,
    for<'q> <DB as Database>::Arguments<'q>: IntoArguments<'q, DB>,
    for<'q> &'q str: Encode<'q, DB> + Type<DB>,
    for<'q> &'q [u8]: Encode<'q, DB> + Type<DB>,
    for<'q> String: Encode<'q, DB> + Type<DB>,
    for<'q> Option<&'q str>: Encode<'q, DB> + Type<DB>,
    for<'q> i64: Encode<'q, DB> + Type<DB>,
    usize: ColumnIndex<DB::Row>,
    for<'r> String: Decode<'r, DB> + Type<DB>,
    for<'r> Option<String>: Decode<'r, DB> + Type<DB>,
    for<'r> Vec<u8>: Decode<'r, DB> + Type<DB>,
    for<'r> i64: Decode<'r, DB> + Type<DB>,
    for<'r> i32: Decode<'r, DB> + Type<DB>,
{
    async fn verify_scope(&self, scope: &TenantQueryScope) -> Result<(), TenantStoreError> {
        verify_lease(self.lease_store.as_ref(), scope).await?;
        let sql = DB::adapt_sql(
            "SELECT holder_spiffe_id, holder_incarnation_pubkey, generation
             FROM tenant_placement_fences WHERE tenant_id = $1",
        );
        let row = sqlx::query::<DB>(&sql)
            .bind(scope.tenant_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("fence select failed: {err}")))?;

        let Some(row) = row else {
            return Err(TenantStoreError::UnauthorizedHolder(
                scope.tenant_id.clone(),
            ));
        };
        let spiffe: String = row.get(0);
        let pk: Vec<u8> = row.get(1);
        let gen_i64: i64 = row.get(2);
        verify_fence_match(scope, &spiffe, &pk, gen_i64)
    }

    async fn provision_tenant(&self, tenant_id: &TenantId) -> Result<(), TenantStoreError> {
        let sql = DB::adapt_sql(
            "INSERT INTO tenant_registry_tenants (tenant_id) VALUES ($1) ON CONFLICT DO NOTHING",
        );
        let res = sqlx::query::<DB>(&sql)
            .bind(tenant_id.as_str())
            .execute(&self.pool)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("provision_tenant failed: {err}")))?;
        if DB::rows_affected(&res) == 0 {
            return Err(TenantStoreError::TenantAlreadyExists(tenant_id.clone()));
        }
        Ok(())
    }

    async fn open_tenant(
        &self,
        scope: &TenantQueryScope,
    ) -> Result<TenantDbHandle, TenantStoreError> {
        let check_sql = DB::adapt_sql("SELECT 1 FROM tenant_registry_tenants WHERE tenant_id = $1");
        let exists: Option<i32> = sqlx::query_scalar::<DB, i32>(&check_sql)
            .bind(scope.tenant_id.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("tenant check failed: {err}")))?;
        if exists.is_none() {
            return Err(TenantStoreError::TenantNotFound(scope.tenant_id.clone()));
        }

        verify_lease(self.lease_store.as_ref(), scope).await?;

        let gen_i64 = i64::try_from(scope.placement_generation.get())
            .map_err(|_| TenantStoreError::Backend("placement generation overflow".to_owned()))?;
        self.upsert_open_fence(scope, gen_i64).await?;

        Ok(TenantDbHandle {
            tenant_id: scope.tenant_id.clone(),
            backend: StorageBackendKind::Postgres,
            placement_generation: scope.placement_generation,
        })
    }

    async fn upsert_open_fence(
        &self,
        scope: &TenantQueryScope,
        gen_i64: i64,
    ) -> Result<(), TenantStoreError> {
        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|err| TenantStoreError::Backend(format!("begin tx failed: {err}")))?;
        let select_sql = DB::adapt_sql(
            "SELECT holder_spiffe_id, holder_incarnation_pubkey, generation
             FROM tenant_placement_fences WHERE tenant_id = $1 FOR UPDATE",
        );
        let existing = sqlx::query::<DB>(&select_sql)
            .bind(scope.tenant_id.as_str())
            .fetch_optional(&mut *tx)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("fence lock failed: {err}")))?;

        let write_sql = if let Some(row) = existing {
            let spiffe: String = row.get(0);
            let pk: Vec<u8> = row.get(1);
            let current_gen_i64: i64 = row.get(2);
            verify_fence_advance(scope, &spiffe, &pk, current_gen_i64)?;
            DB::adapt_sql(
                "UPDATE tenant_placement_fences
                 SET holder_spiffe_id = $2,
                     holder_incarnation_pubkey = $3,
                     generation = $4,
                     updated_at = now()
                 WHERE tenant_id = $1 AND generation <= $4",
            )
        } else {
            DB::adapt_sql(
                "INSERT INTO tenant_placement_fences
                 (tenant_id, holder_spiffe_id, holder_incarnation_pubkey, generation)
                 VALUES ($1, $2, $3, $4)",
            )
        };
        sqlx::query::<DB>(&write_sql)
            .bind(scope.tenant_id.as_str())
            .bind(scope.holder.spiffe_id.as_str())
            .bind(scope.holder.incarnation_pubkey.as_bytes().as_slice())
            .bind(gen_i64)
            .execute(&mut *tx)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("fence write failed: {err}")))?;

        tx.commit()
            .await
            .map_err(|err| TenantStoreError::Backend(format!("commit failed: {err}")))?;
        Ok(())
    }

    async fn delete_tenant(&self, tenant_id: &TenantId) -> Result<(), TenantStoreError> {
        let sql = DB::adapt_sql("DELETE FROM tenant_registry_tenants WHERE tenant_id = $1");
        sqlx::query::<DB>(&sql)
            .bind(tenant_id.as_str())
            .execute(&self.pool)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("delete_tenant failed: {err}")))?;
        Ok(())
    }

    async fn upsert_user(
        &self,
        scope: &TenantQueryScope,
        user: &TenantUserRecord,
    ) -> Result<(), TenantStoreError> {
        self.verify_scope(scope).await?;
        let sql = DB::adapt_sql(
            "INSERT INTO tenant_users (tenant_id, jid, account_id, username, xmpp_localpart, display_name)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (tenant_id, jid) DO UPDATE SET
                 username = EXCLUDED.username,
                 xmpp_localpart = EXCLUDED.xmpp_localpart,
                 display_name = EXCLUDED.display_name",
        );
        sqlx::query::<DB>(&sql)
            .bind(scope.tenant_id.as_str())
            .bind(user.jid.as_str())
            .bind(user.account_id.to_string())
            .bind(user.username.as_str())
            .bind(user.xmpp_localpart.as_str())
            .bind(user.display_name.as_deref())
            .execute(&self.pool)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("upsert_user failed: {err}")))?;
        Ok(())
    }

    async fn get_user_by_localpart(
        &self,
        scope: &TenantQueryScope,
        xmpp_localpart: &str,
    ) -> Result<Option<TenantUserRecord>, TenantStoreError> {
        self.verify_scope(scope).await?;
        let sql = DB::adapt_sql(
            "SELECT jid, account_id, username, xmpp_localpart, display_name
             FROM tenant_users WHERE tenant_id = $1 AND xmpp_localpart = $2",
        );
        let row = sqlx::query::<DB>(&sql)
            .bind(scope.tenant_id.as_str())
            .bind(xmpp_localpart)
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("get_user failed: {err}")))?;
        row.as_ref().map(decode_user_row::<DB>).transpose()
    }

    async fn update_user_account_id_for_test(
        &self,
        scope: &TenantQueryScope,
        jid: &str,
        new_account_id: AccountId,
    ) -> Result<(), TenantStoreError> {
        self.verify_scope(scope).await?;
        let sql = DB::adapt_sql(
            "UPDATE tenant_users SET account_id = $3 WHERE tenant_id = $1 AND jid = $2",
        );
        sqlx::query::<DB>(&sql)
            .bind(scope.tenant_id.as_str())
            .bind(jid)
            .bind(new_account_id.to_string())
            .execute(&self.pool)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("update account_id failed: {err}")))?;
        Ok(())
    }

    async fn upsert_auth_identity(
        &self,
        scope: &TenantQueryScope,
        identity: &TenantAuthIdentityRecord,
    ) -> Result<(), TenantStoreError> {
        self.verify_scope(scope).await?;
        let sql = DB::adapt_sql(
            "INSERT INTO tenant_auth_identities (tenant_id, id, user_jid, provider_id, issuer, subject)
             VALUES ($1, $2, $3, $4, $5, $6)
             ON CONFLICT (tenant_id, issuer, subject) DO UPDATE SET
                 user_jid = EXCLUDED.user_jid,
                 provider_id = EXCLUDED.provider_id",
        );
        sqlx::query::<DB>(&sql)
            .bind(scope.tenant_id.as_str())
            .bind(identity.id.as_str())
            .bind(identity.user_jid.as_str())
            .bind(identity.provider_id.as_str())
            .bind(identity.issuer.as_str())
            .bind(identity.subject.as_str())
            .execute(&self.pool)
            .await
            .map_err(|err| {
                TenantStoreError::Backend(format!("upsert_auth_identity failed: {err}"))
            })?;
        Ok(())
    }

    async fn get_auth_identity(
        &self,
        scope: &TenantQueryScope,
        issuer: &str,
        subject: &str,
    ) -> Result<Option<TenantAuthIdentityRecord>, TenantStoreError> {
        self.verify_scope(scope).await?;
        let sql = DB::adapt_sql(
            "SELECT id, user_jid, provider_id, issuer, subject
             FROM tenant_auth_identities
             WHERE tenant_id = $1 AND issuer = $2 AND subject = $3",
        );
        let row = sqlx::query::<DB>(&sql)
            .bind(scope.tenant_id.as_str())
            .bind(issuer)
            .bind(subject)
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("get_auth_identity failed: {err}")))?;
        Ok(row.as_ref().map(decode_auth_identity_row::<DB>))
    }

    async fn upsert_space(
        &self,
        scope: &TenantQueryScope,
        space: &TenantSpaceRecord,
    ) -> Result<(), TenantStoreError> {
        self.verify_scope(scope).await?;
        let sql = DB::adapt_sql(
            "INSERT INTO tenant_spaces_metadata (tenant_id, space_node, space_jid, name)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (tenant_id, space_node) DO UPDATE SET
                 space_jid = EXCLUDED.space_jid,
                 name = EXCLUDED.name",
        );
        sqlx::query::<DB>(&sql)
            .bind(scope.tenant_id.as_str())
            .bind(space.space_node.as_str())
            .bind(space.space_jid.as_str())
            .bind(space.name.as_str())
            .execute(&self.pool)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("upsert_space failed: {err}")))?;
        Ok(())
    }

    async fn get_space(
        &self,
        scope: &TenantQueryScope,
        space_node: &str,
    ) -> Result<Option<TenantSpaceRecord>, TenantStoreError> {
        self.verify_scope(scope).await?;
        let sql = DB::adapt_sql(
            "SELECT space_node, space_jid, name
             FROM tenant_spaces_metadata WHERE tenant_id = $1 AND space_node = $2",
        );
        let row = sqlx::query::<DB>(&sql)
            .bind(scope.tenant_id.as_str())
            .bind(space_node)
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("get_space failed: {err}")))?;
        Ok(row.as_ref().map(decode_space_row::<DB>))
    }

    async fn record_remote_participant(
        &self,
        scope: &TenantQueryScope,
        participant: &RemoteParticipantRecord,
    ) -> Result<(), TenantStoreError> {
        self.verify_scope(scope).await?;
        let sql = DB::adapt_sql(
            "INSERT INTO tenant_remote_participants (tenant_id, room_id, remote_bare_jid, affiliation)
             VALUES ($1, $2, $3, $4)
             ON CONFLICT (tenant_id, room_id, remote_bare_jid) DO UPDATE SET
                 affiliation = EXCLUDED.affiliation",
        );
        sqlx::query::<DB>(&sql)
            .bind(scope.tenant_id.as_str())
            .bind(participant.room_id.as_str())
            .bind(participant.remote_bare_jid.as_str())
            .bind(participant.affiliation.as_str())
            .execute(&self.pool)
            .await
            .map_err(|err| {
                TenantStoreError::Backend(format!("record_remote_participant failed: {err}"))
            })?;
        Ok(())
    }

    async fn list_remote_participants(
        &self,
        scope: &TenantQueryScope,
        room_id: &str,
    ) -> Result<Vec<RemoteParticipantRecord>, TenantStoreError> {
        self.verify_scope(scope).await?;
        let sql = DB::adapt_sql(
            "SELECT room_id, remote_bare_jid, affiliation
             FROM tenant_remote_participants
             WHERE tenant_id = $1 AND room_id = $2
             ORDER BY remote_bare_jid",
        );
        let rows = sqlx::query::<DB>(&sql)
            .bind(scope.tenant_id.as_str())
            .bind(room_id)
            .fetch_all(&self.pool)
            .await
            .map_err(|err| {
                TenantStoreError::Backend(format!("list_remote_participants failed: {err}"))
            })?;
        Ok(rows
            .iter()
            .map(decode_remote_participant_row::<DB>)
            .collect())
    }

    async fn enqueue_job(
        &self,
        scope: &TenantQueryScope,
        job: &TenantJobRecord,
    ) -> Result<(), TenantStoreError> {
        self.verify_scope(scope).await?;
        let sql = DB::adapt_sql(
            "INSERT INTO tenant_jobs (tenant_id, job_id, queue, payload_json, status)
             VALUES ($1, $2, $3, $4, $5)",
        );
        sqlx::query::<DB>(&sql)
            .bind(scope.tenant_id.as_str())
            .bind(job.job_id.as_str())
            .bind(job.queue.as_str())
            .bind(job.payload_json.as_str())
            .bind(job.status.as_str())
            .execute(&self.pool)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("enqueue_job failed: {err}")))?;
        Ok(())
    }

    async fn claim_next_job(
        &self,
        scope: &TenantQueryScope,
        queue: &str,
    ) -> Result<Option<TenantJobRecord>, TenantStoreError> {
        self.verify_scope(scope).await?;
        DB::claim_next_job_raw(&self.pool, scope.tenant_id.as_str(), queue).await
    }
}

macro_rules! delegate_tenant_store_traits {
    ($store:ty) => {
        #[async_trait]
        impl TenantStore for $store {
            fn backend_kind(&self) -> StorageBackendKind {
                StorageBackendKind::Postgres
            }

            async fn provision_tenant(&self, tenant_id: &TenantId) -> Result<(), TenantStoreError> {
                self.core.provision_tenant(tenant_id).await
            }

            async fn open_tenant(
                &self,
                scope: &TenantQueryScope,
            ) -> Result<TenantDbHandle, TenantStoreError> {
                self.core.open_tenant(scope).await
            }

            async fn verify_write_fence(
                &self,
                scope: &TenantQueryScope,
            ) -> Result<(), TenantStoreError> {
                self.core.verify_scope(scope).await
            }

            async fn checkpoint_and_release(
                &self,
                _tenant_id: &TenantId,
            ) -> Result<(), TenantStoreError> {
                Ok(())
            }

            async fn delete_tenant(&self, tenant_id: &TenantId) -> Result<(), TenantStoreError> {
                self.core.delete_tenant(tenant_id).await
            }
        }

        #[async_trait]
        impl TenantDataOps for $store {
            async fn upsert_user(
                &self,
                scope: &TenantQueryScope,
                user: &TenantUserRecord,
            ) -> Result<(), TenantStoreError> {
                self.core.upsert_user(scope, user).await
            }

            async fn get_user_by_localpart(
                &self,
                scope: &TenantQueryScope,
                xmpp_localpart: &str,
            ) -> Result<Option<TenantUserRecord>, TenantStoreError> {
                self.core.get_user_by_localpart(scope, xmpp_localpart).await
            }

            async fn update_user_account_id_for_test(
                &self,
                scope: &TenantQueryScope,
                jid: &str,
                new_account_id: AccountId,
            ) -> Result<(), TenantStoreError> {
                self.core
                    .update_user_account_id_for_test(scope, jid, new_account_id)
                    .await
            }

            async fn upsert_auth_identity(
                &self,
                scope: &TenantQueryScope,
                identity: &TenantAuthIdentityRecord,
            ) -> Result<(), TenantStoreError> {
                self.core.upsert_auth_identity(scope, identity).await
            }

            async fn get_auth_identity(
                &self,
                scope: &TenantQueryScope,
                issuer: &str,
                subject: &str,
            ) -> Result<Option<TenantAuthIdentityRecord>, TenantStoreError> {
                self.core.get_auth_identity(scope, issuer, subject).await
            }

            async fn upsert_space(
                &self,
                scope: &TenantQueryScope,
                space: &TenantSpaceRecord,
            ) -> Result<(), TenantStoreError> {
                self.core.upsert_space(scope, space).await
            }

            async fn get_space(
                &self,
                scope: &TenantQueryScope,
                space_node: &str,
            ) -> Result<Option<TenantSpaceRecord>, TenantStoreError> {
                self.core.get_space(scope, space_node).await
            }

            async fn record_remote_participant(
                &self,
                scope: &TenantQueryScope,
                participant: &RemoteParticipantRecord,
            ) -> Result<(), TenantStoreError> {
                self.core
                    .record_remote_participant(scope, participant)
                    .await
            }

            async fn list_remote_participants(
                &self,
                scope: &TenantQueryScope,
                room_id: &str,
            ) -> Result<Vec<RemoteParticipantRecord>, TenantStoreError> {
                self.core.list_remote_participants(scope, room_id).await
            }

            async fn enqueue_job(
                &self,
                scope: &TenantQueryScope,
                job: &TenantJobRecord,
            ) -> Result<(), TenantStoreError> {
                self.core.enqueue_job(scope, job).await
            }

            async fn claim_next_job(
                &self,
                scope: &TenantQueryScope,
                queue: &str,
            ) -> Result<Option<TenantJobRecord>, TenantStoreError> {
                self.core.claim_next_job(scope, queue).await
            }
        }
    };
}

// ============================================================================
// Production PostgreSQL store
// ============================================================================

/// Production multi-tenant PostgreSQL [`TenantStore`] with mandatory `tenant_id`
/// predicates and composite unique keys across all tenant tables.
pub struct PostgresTenantStore {
    core: SqlTenantStoreCore<Postgres>,
}

impl PostgresTenantStore {
    pub fn new(pool: PgPool, lease_store: Option<Arc<dyn PlacementLeaseStore>>) -> Self {
        Self {
            core: SqlTenantStoreCore { pool, lease_store },
        }
    }

    pub async fn ensure_schema(&self) -> Result<(), TenantStoreError> {
        for stmt in PG_HEAD_SCHEMA
            .iter()
            .chain(SHARED_DATA_SCHEMA)
            .chain(PG_TAIL_SCHEMA)
        {
            sqlx::query(stmt)
                .execute(&self.core.pool)
                .await
                .map_err(|err| TenantStoreError::Backend(format!("postgres DDL failed: {err}")))?;
        }
        Ok(())
    }
}

delegate_tenant_store_traits!(PostgresTenantStore);

// ============================================================================
// In-process SQLite-compat multi-tenant Postgres schema contract adapter
// ============================================================================

/// In-process SQLite-backed adapter mirroring [`PostgresTenantStore`]'s exact
/// multi-tenant `tenant_id`-scoped schema and composite unique keys so the
/// shared contract suite runs unconditionally on machines without PostgreSQL.
pub struct SqliteCompatPostgresStore {
    core: SqlTenantStoreCore<Sqlite>,
}

impl SqliteCompatPostgresStore {
    pub async fn new_in_memory(
        lease_store: Option<Arc<dyn PlacementLeaseStore>>,
    ) -> Result<Self, TenantStoreError> {
        let pool = SqlitePool::connect("sqlite::memory:")
            .await
            .map_err(|err| TenantStoreError::Backend(format!("sqlite connect failed: {err}")))?;
        for stmt in SQLITE_HEAD_SCHEMA
            .iter()
            .chain(SHARED_DATA_SCHEMA)
            .chain(SQLITE_TAIL_SCHEMA)
        {
            sqlx::query(stmt)
                .execute(&pool)
                .await
                .map_err(|err| TenantStoreError::Backend(format!("sqlite DDL failed: {err}")))?;
        }
        Ok(Self {
            core: SqlTenantStoreCore { pool, lease_store },
        })
    }
}

delegate_tenant_store_traits!(SqliteCompatPostgresStore);
