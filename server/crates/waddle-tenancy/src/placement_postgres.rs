//! PostgreSQL [`PlacementLeaseStore`] (`PostgresPlacementLeaseStore` using
//! `sqlx::PgPool`, PostgreSQL `now()`, and `FOR SHARE` write fencing) plus an
//! in-process SQLite-compat placement store (`SqliteCompatPlacementLeaseStore`)
//! so all 7 placement & fencing scenarios run unconditionally on hosts without
//! a local Postgres daemon.

use std::borrow::Cow;

use async_trait::async_trait;
use sqlx::sqlite::SqlitePoolOptions;
use sqlx::{
    ColumnIndex, Database, Decode, Encode, Executor, IntoArguments, PgPool, Pool, Postgres, Row,
    Sqlite, Transaction, Type,
};
use waddle_broker_core::identity::{
    CellRole, HolderInstance, IncarnationPubKey, SpiffeId, TenantId, TlsExporterProof,
};
use waddle_broker_core::placement::{
    FenceGeneration, LeaseError, PlacementLease, PlacementLeaseStore, PlacementMode, TransferState,
};
use waddle_broker_core::store::{TenantQueryScope, TenantStoreError};

use crate::placement_turso::MAX_LEASE_TTL_MS;
use crate::spiffe_x509::spiffe_matches_cell_role;

const PG_PLACEMENT_SCHEMA: &str = r#"CREATE TABLE IF NOT EXISTS tenant_placement_leases (
    tenant_id                 TEXT PRIMARY KEY,
    cell_role                 TEXT NOT NULL,
    placement_mode            TEXT NOT NULL CHECK (placement_mode IN ('shared_pool', 'dedicated_cell')),
    holder_spiffe_id          TEXT NOT NULL,
    holder_incarnation_pubkey BYTEA NOT NULL CHECK (octet_length(holder_incarnation_pubkey) = 32),
    holder_tls_exporter_proof BYTEA NOT NULL CHECK (octet_length(holder_tls_exporter_proof) BETWEEN 1 AND 128),
    generation                BIGINT NOT NULL CHECK (generation >= 1),
    expires_at                TIMESTAMPTZ NOT NULL,
    transfer_state            TEXT NOT NULL CHECK (transfer_state IN ('active', 'transferring', 'draining', 'released')),
    transfer_target_json      JSONB,
    updated_at                TIMESTAMPTZ NOT NULL DEFAULT now()
)"#;

const SQLITE_NOW_MS_EXPR: &str = "CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER)";

/// SQL statement used inside a PostgreSQL `EnteredTransaction` to take a
/// `FOR SHARE` row lock on the active placement lease, preventing concurrent
/// `transfer`/`release` until the data transaction commits or rolls back.
pub const ASSERT_PLACEMENT_LEASE_FOR_SHARE_SQL: &str = r#"SELECT 1 FROM tenant_placement_leases
WHERE tenant_id = $1
  AND holder_spiffe_id = $2
  AND holder_incarnation_pubkey = $3
  AND generation = $4
  AND transfer_state IN ('active', 'draining')
  AND expires_at > now()
FOR SHARE"#;

// ============================================================================
// Shared validation, row decoding, CAS classification, and SQL backend trait
// ============================================================================

fn validate_ttl_ms(ttl_ms: u64) -> Result<i64, LeaseError> {
    if ttl_ms == 0 || ttl_ms > MAX_LEASE_TTL_MS {
        return Err(LeaseError::InvalidTtl(ttl_ms));
    }
    i64::try_from(ttl_ms).map_err(|_| LeaseError::InvalidTtl(ttl_ms))
}

fn generation_to_i64(generation: FenceGeneration) -> Result<i64, LeaseError> {
    i64::try_from(generation.get())
        .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))
}

fn mode_to_str(mode: PlacementMode) -> &'static str {
    match mode {
        PlacementMode::SharedPool => "shared_pool",
        PlacementMode::DedicatedCell => "dedicated_cell",
    }
}

fn parse_cell_role_and_mode(
    role_str: &str,
    mode_str: &str,
) -> Result<(CellRole, PlacementMode), LeaseError> {
    let cell_role = CellRole::parse(role_str)
        .map_err(|e| LeaseError::Backend(format!("invalid stored cell_role: {e}")))?;
    let mode = match mode_str {
        "shared_pool" => PlacementMode::SharedPool,
        "dedicated_cell" => PlacementMode::DedicatedCell,
        other => {
            return Err(LeaseError::Backend(format!(
                "unknown placement_mode: {other}"
            )));
        }
    };
    Ok((cell_role, mode))
}

fn verify_holder_role(
    tenant: &TenantId,
    holder: &HolderInstance,
    role_str: &str,
    mode_str: &str,
) -> Result<(), LeaseError> {
    let (role, mode) = parse_cell_role_and_mode(role_str, mode_str)?;
    if !spiffe_matches_cell_role(&holder.spiffe_id, &role, mode) {
        return Err(LeaseError::HolderMismatch(tenant.clone()));
    }
    Ok(())
}

fn check_write_fence_row(
    scope: &TenantQueryScope,
    row: Option<i32>,
) -> Result<(), TenantStoreError> {
    if row.is_some() {
        Ok(())
    } else {
        Err(TenantStoreError::StalePlacementGeneration {
            tenant_id: scope.tenant_id.clone(),
            expected: scope.placement_generation,
            actual: FenceGeneration::ZERO,
        })
    }
}

enum RawTransferTarget {
    JsonValue(Option<serde_json::Value>),
    JsonStr(Option<String>),
}

impl RawTransferTarget {
    fn parse_holder(self) -> Result<HolderInstance, LeaseError> {
        match self {
            Self::JsonValue(val) => {
                let val = val.ok_or_else(|| {
                    LeaseError::Backend("transferring missing target json".to_owned())
                })?;
                serde_json::from_value(val)
                    .map_err(|e| LeaseError::Backend(format!("invalid transfer target: {e}")))
            }
            Self::JsonStr(raw) => {
                let raw =
                    raw.ok_or_else(|| LeaseError::Backend("missing target_json".to_owned()))?;
                serde_json::from_str(&raw)
                    .map_err(|e| LeaseError::Backend(format!("invalid target_json: {e}")))
            }
        }
    }
}

fn decode_lease_row<DB>(tenant: &TenantId, row: &DB::Row) -> Result<PlacementLease, LeaseError>
where
    DB: PlacementSqlDb,
    for<'c> &'c str: ColumnIndex<DB::Row>,
    for<'r> String: Decode<'r, DB> + Type<DB>,
    for<'r> Vec<u8>: Decode<'r, DB> + Type<DB>,
    for<'r> i64: Decode<'r, DB> + Type<DB>,
{
    let role_str: String = row.get("cell_role");
    let mode_str: String = row.get("placement_mode");
    let spiffe_str: String = row.get("holder_spiffe_id");
    let pk_vec: Vec<u8> = row.get("holder_incarnation_pubkey");
    let proof_vec: Vec<u8> = row.get("holder_tls_exporter_proof");
    let gen_i64: i64 = row.get("generation");
    let expires_at_ms: i64 = row.get("expires_at_ms");
    let state_str: String = row.get("transfer_state");
    let target = DB::read_transfer_target(row);

    let (cell_role, mode) = parse_cell_role_and_mode(&role_str, &mode_str)?;
    let spiffe_id = SpiffeId::parse(&spiffe_str)
        .map_err(|e| LeaseError::Backend(format!("invalid stored spiffe_id: {e}")))?;
    let pk: [u8; 32] = pk_vec
        .try_into()
        .map_err(|_| LeaseError::Backend("stored pubkey != 32 bytes".to_owned()))?;
    let proof = TlsExporterProof::new(proof_vec)
        .map_err(|e| LeaseError::Backend(format!("invalid stored proof: {e}")))?;
    let transfer_state = match state_str.as_str() {
        "active" => TransferState::Active,
        "draining" => TransferState::Draining,
        "transferring" => TransferState::Transferring {
            target: target.parse_holder()?,
        },
        other => return Err(LeaseError::Backend(format!("unexpected state: {other}"))),
    };
    let gen = u64::try_from(gen_i64)
        .map_err(|_| LeaseError::Backend("negative generation".to_owned()))?;
    Ok(PlacementLease {
        tenant_id: tenant.clone(),
        cell_role,
        mode,
        holder: HolderInstance {
            spiffe_id,
            incarnation_pubkey: IncarnationPubKey::from_bytes(pk),
            tls_exporter_proof: proof,
        },
        generation: FenceGeneration::new(gen),
        expires_at_ms,
        transfer_state,
    })
}

fn adapt_pg_placement_sql_for_sqlite(pg_sql: &str) -> String {
    pg_sql
        .replace(
            "FLOOR(EXTRACT(EPOCH FROM expires_at) * 1000)::BIGINT AS expires_at_ms",
            "expires_at",
        )
        .replace("make_interval(secs => (", "")
        .replace("::double precision / 1000.0))", "")
        .replace("to_timestamp(0)", "0")
        .replace("expires_at", "expires_at_ms")
        .replace("updated_at", "updated_at_ms")
        .replace("now()", SQLITE_NOW_MS_EXPR)
        .replace("FOR SHARE", "")
        .replace('$', "?")
}

fn sqlite_placement_schema() -> String {
    PG_PLACEMENT_SCHEMA
        .replace("BYTEA", "BLOB")
        .replace("octet_length(", "length(")
        .replace("BIGINT", "INTEGER")
        .replace(
            "expires_at                TIMESTAMPTZ",
            "expires_at_ms             INTEGER",
        )
        .replace("JSONB", "TEXT")
        .replace(
            "updated_at                TIMESTAMPTZ NOT NULL DEFAULT now()",
            "updated_at_ms             INTEGER NOT NULL",
        )
}

trait PlacementSqlDb: Database {
    const LABEL: &'static str;

    fn adapt_sql(pg_sql: &'static str) -> Cow<'static, str>;

    fn rows_affected(result: &Self::QueryResult) -> u64;

    fn read_transfer_target(row: &Self::Row) -> RawTransferTarget;
}

impl PlacementSqlDb for Postgres {
    const LABEL: &'static str = "pg";

    fn adapt_sql(pg_sql: &'static str) -> Cow<'static, str> {
        Cow::Borrowed(pg_sql)
    }

    fn rows_affected(result: &Self::QueryResult) -> u64 {
        result.rows_affected()
    }

    fn read_transfer_target(row: &Self::Row) -> RawTransferTarget {
        RawTransferTarget::JsonValue(row.get("transfer_target_json"))
    }
}

impl PlacementSqlDb for Sqlite {
    const LABEL: &'static str = "compat";

    fn adapt_sql(pg_sql: &'static str) -> Cow<'static, str> {
        Cow::Owned(adapt_pg_placement_sql_for_sqlite(pg_sql))
    }

    fn rows_affected(result: &Self::QueryResult) -> u64 {
        result.rows_affected()
    }

    fn read_transfer_target(row: &Self::Row) -> RawTransferTarget {
        RawTransferTarget::JsonStr(row.get("transfer_target_json"))
    }
}

struct PlacementLeaseCore<DB: Database> {
    pool: Pool<DB>,
}

impl<DB> PlacementLeaseCore<DB>
where
    DB: PlacementSqlDb,
    for<'c> &'c Pool<DB>: Executor<'c, Database = DB>,
    for<'q> <DB as Database>::Arguments<'q>: IntoArguments<'q, DB>,
    for<'q> &'q str: Encode<'q, DB> + Type<DB>,
    for<'q> &'q [u8]: Encode<'q, DB> + Type<DB>,
    for<'q> i64: Encode<'q, DB> + Type<DB>,
    usize: ColumnIndex<DB::Row>,
    for<'c> &'c str: ColumnIndex<DB::Row>,
    for<'r> String: Decode<'r, DB> + Type<DB>,
    for<'r> Vec<u8>: Decode<'r, DB> + Type<DB>,
    for<'r> i64: Decode<'r, DB> + Type<DB>,
    for<'r> bool: Decode<'r, DB> + Type<DB>,
{
    fn new(pool: Pool<DB>) -> Self {
        Self { pool }
    }

    async fn classify_cas_failure(
        &self,
        tenant: &TenantId,
        holder: &HolderInstance,
        expected_generation: FenceGeneration,
    ) -> LeaseError {
        let sql = DB::adapt_sql(
            "SELECT holder_spiffe_id, holder_incarnation_pubkey, generation,
                    transfer_state, (expires_at <= now()) AS expired
             FROM tenant_placement_leases WHERE tenant_id = $1",
        );
        let row = sqlx::query::<DB>(&sql)
            .bind(tenant.as_str())
            .fetch_optional(&self.pool)
            .await;

        let row = match row {
            Ok(Some(r)) => r,
            Ok(None) => return LeaseError::NotFound(tenant.clone()),
            Err(err) => {
                return LeaseError::Backend(format!("{} classify failed: {err}", DB::LABEL))
            }
        };
        let actual_gen = FenceGeneration::new(u64::try_from(row.get::<i64, _>(2)).unwrap_or(0));
        if actual_gen != expected_generation {
            return LeaseError::StaleGeneration {
                tenant_id: tenant.clone(),
                expected: expected_generation,
                actual: actual_gen,
            };
        }
        let spiffe: String = row.get(0);
        let pk: Vec<u8> = row.get(1);
        if spiffe != holder.spiffe_id.as_str()
            || pk.as_slice() != holder.incarnation_pubkey.as_bytes()
        {
            return LeaseError::HolderMismatch(tenant.clone());
        }
        let state: String = row.get(3);
        let expired: bool = row.get(4);
        if state == "released" || expired {
            return LeaseError::LeaseExpired(tenant.clone());
        }
        LeaseError::Backend(format!("{} placement CAS rejected", DB::LABEL))
    }

    async fn mark_draining(
        &self,
        tenant: &TenantId,
        holder: &HolderInstance,
        expected_generation: FenceGeneration,
    ) -> Result<(), LeaseError> {
        let gen_i64 = generation_to_i64(expected_generation)?;
        let sql = DB::adapt_sql(
            "UPDATE tenant_placement_leases
             SET transfer_state = 'draining', updated_at = now()
             WHERE tenant_id = $1
               AND holder_spiffe_id = $2
               AND holder_incarnation_pubkey = $3
               AND generation = $4
               AND transfer_state IN ('active', 'draining')
               AND expires_at > now()",
        );
        let res = sqlx::query::<DB>(&sql)
            .bind(tenant.as_str())
            .bind(holder.spiffe_id.as_str())
            .bind(holder.incarnation_pubkey.as_bytes().as_slice())
            .bind(gen_i64)
            .execute(&self.pool)
            .await
            .map_err(|err| {
                LeaseError::Backend(format!("{} mark_draining failed: {err}", DB::LABEL))
            })?;

        if DB::rows_affected(&res) == 1 {
            Ok(())
        } else {
            Err(self
                .classify_cas_failure(tenant, holder, expected_generation)
                .await)
        }
    }

    async fn claim(
        &self,
        tenant: &TenantId,
        role: &CellRole,
        mode: PlacementMode,
        holder: &HolderInstance,
        ttl_ms: u64,
    ) -> Result<PlacementLease, LeaseError> {
        let ttl_i64 = validate_ttl_ms(ttl_ms)?;
        if !spiffe_matches_cell_role(&holder.spiffe_id, role, mode) {
            return Err(LeaseError::HolderMismatch(tenant.clone()));
        }

        let sql = DB::adapt_sql(
            "INSERT INTO tenant_placement_leases
             (tenant_id, cell_role, placement_mode, holder_spiffe_id,
              holder_incarnation_pubkey, holder_tls_exporter_proof,
              generation, expires_at, transfer_state, transfer_target_json, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, 1, now() + make_interval(secs => ($7::double precision / 1000.0)), 'active', NULL, now())
             ON CONFLICT (tenant_id) DO UPDATE SET
                 cell_role = EXCLUDED.cell_role,
                 placement_mode = EXCLUDED.placement_mode,
                 holder_spiffe_id = EXCLUDED.holder_spiffe_id,
                 holder_incarnation_pubkey = EXCLUDED.holder_incarnation_pubkey,
                 holder_tls_exporter_proof = EXCLUDED.holder_tls_exporter_proof,
                 generation = tenant_placement_leases.generation + 1,
                 expires_at = now() + make_interval(secs => ($7::double precision / 1000.0)),
                 transfer_state = 'active',
                 transfer_target_json = NULL,
                 updated_at = now()
             WHERE tenant_placement_leases.transfer_state = 'released'
                OR tenant_placement_leases.expires_at <= now()
             RETURNING cell_role, placement_mode, holder_spiffe_id, holder_incarnation_pubkey,
                       holder_tls_exporter_proof, generation,
                       FLOOR(EXTRACT(EPOCH FROM expires_at) * 1000)::BIGINT AS expires_at_ms,
                       transfer_state, transfer_target_json",
        );
        let row = sqlx::query::<DB>(&sql)
            .bind(tenant.as_str())
            .bind(role.as_str())
            .bind(mode_to_str(mode))
            .bind(holder.spiffe_id.as_str())
            .bind(holder.incarnation_pubkey.as_bytes().as_slice())
            .bind(holder.tls_exporter_proof.as_bytes())
            .bind(ttl_i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| LeaseError::Backend(format!("{} claim failed: {err}", DB::LABEL)))?;

        if let Some(row) = row {
            decode_lease_row::<DB>(tenant, &row)
        } else {
            let gen_sql = DB::adapt_sql(
                "SELECT generation FROM tenant_placement_leases WHERE tenant_id = $1",
            );
            let gen_i64: i64 = sqlx::query_scalar::<DB, i64>(&gen_sql)
                .bind(tenant.as_str())
                .fetch_one(&self.pool)
                .await
                .map_err(|err| {
                    LeaseError::Backend(format!("{} read gen failed: {err}", DB::LABEL))
                })?;
            Err(LeaseError::AlreadyHeld {
                tenant_id: tenant.clone(),
                current_generation: FenceGeneration::new(u64::try_from(gen_i64).unwrap_or(0)),
            })
        }
    }

    async fn renew(
        &self,
        tenant: &TenantId,
        holder: &HolderInstance,
        generation: FenceGeneration,
        ttl_ms: u64,
    ) -> Result<PlacementLease, LeaseError> {
        let ttl_i64 = validate_ttl_ms(ttl_ms)?;
        let gen_i64 = generation_to_i64(generation)?;
        let sql = DB::adapt_sql(
            "UPDATE tenant_placement_leases
             SET expires_at = now() + make_interval(secs => ($5::double precision / 1000.0)),
                 holder_tls_exporter_proof = $6,
                 updated_at = now()
             WHERE tenant_id = $1
               AND holder_spiffe_id = $2
               AND holder_incarnation_pubkey = $3
               AND generation = $4
               AND transfer_state IN ('active', 'draining', 'transferring')
               AND expires_at > now()
             RETURNING cell_role, placement_mode, holder_spiffe_id, holder_incarnation_pubkey,
                       holder_tls_exporter_proof, generation,
                       FLOOR(EXTRACT(EPOCH FROM expires_at) * 1000)::BIGINT AS expires_at_ms,
                       transfer_state, transfer_target_json",
        );
        let row = sqlx::query::<DB>(&sql)
            .bind(tenant.as_str())
            .bind(holder.spiffe_id.as_str())
            .bind(holder.incarnation_pubkey.as_bytes().as_slice())
            .bind(gen_i64)
            .bind(ttl_i64)
            .bind(holder.tls_exporter_proof.as_bytes())
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| LeaseError::Backend(format!("{} renew failed: {err}", DB::LABEL)))?;

        match row {
            Some(row) => decode_lease_row::<DB>(tenant, &row),
            None => Err(self.classify_cas_failure(tenant, holder, generation).await),
        }
    }

    async fn transfer(
        &self,
        tenant: &TenantId,
        from_holder: &HolderInstance,
        to_holder: &HolderInstance,
        expected_generation: FenceGeneration,
        ttl_ms: u64,
    ) -> Result<PlacementLease, LeaseError> {
        let ttl_i64 = validate_ttl_ms(ttl_ms)?;
        let gen_i64 = generation_to_i64(expected_generation)?;

        let meta_sql = DB::adapt_sql(
            "SELECT cell_role, placement_mode FROM tenant_placement_leases WHERE tenant_id = $1",
        );
        let meta = sqlx::query::<DB>(&meta_sql)
            .bind(tenant.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| LeaseError::Backend(format!("{} transfer meta: {err}", DB::LABEL)))?;
        let Some(meta) = meta else {
            return Err(LeaseError::NotFound(tenant.clone()));
        };
        let role_str: String = meta.get(0);
        let mode_str: String = meta.get(1);
        verify_holder_role(tenant, to_holder, &role_str, &mode_str)?;

        let sql = DB::adapt_sql(
            "UPDATE tenant_placement_leases
             SET holder_spiffe_id = $5,
                 holder_incarnation_pubkey = $6,
                 holder_tls_exporter_proof = $7,
                 generation = generation + 1,
                 expires_at = now() + make_interval(secs => ($8::double precision / 1000.0)),
                 transfer_state = 'active',
                 transfer_target_json = NULL,
                 updated_at = now()
             WHERE tenant_id = $1
               AND holder_spiffe_id = $2
               AND holder_incarnation_pubkey = $3
               AND generation = $4
               AND transfer_state != 'released'
               AND expires_at > now()
             RETURNING cell_role, placement_mode, holder_spiffe_id, holder_incarnation_pubkey,
                       holder_tls_exporter_proof, generation,
                       FLOOR(EXTRACT(EPOCH FROM expires_at) * 1000)::BIGINT AS expires_at_ms,
                       transfer_state, transfer_target_json",
        );
        let row = sqlx::query::<DB>(&sql)
            .bind(tenant.as_str())
            .bind(from_holder.spiffe_id.as_str())
            .bind(from_holder.incarnation_pubkey.as_bytes().as_slice())
            .bind(gen_i64)
            .bind(to_holder.spiffe_id.as_str())
            .bind(to_holder.incarnation_pubkey.as_bytes().as_slice())
            .bind(to_holder.tls_exporter_proof.as_bytes())
            .bind(ttl_i64)
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| LeaseError::Backend(format!("{} transfer failed: {err}", DB::LABEL)))?;

        match row {
            Some(row) => decode_lease_row::<DB>(tenant, &row),
            None => Err(self
                .classify_cas_failure(tenant, from_holder, expected_generation)
                .await),
        }
    }

    async fn release(
        &self,
        tenant: &TenantId,
        holder: &HolderInstance,
        expected_generation: FenceGeneration,
    ) -> Result<(), LeaseError> {
        let gen_i64 = generation_to_i64(expected_generation)?;
        let sql = DB::adapt_sql(
            "UPDATE tenant_placement_leases
             SET transfer_state = 'released',
                 expires_at = to_timestamp(0),
                 transfer_target_json = NULL,
                 updated_at = now()
             WHERE tenant_id = $1
               AND holder_spiffe_id = $2
               AND holder_incarnation_pubkey = $3
               AND generation = $4
               AND transfer_state != 'released'",
        );
        let res = sqlx::query::<DB>(&sql)
            .bind(tenant.as_str())
            .bind(holder.spiffe_id.as_str())
            .bind(holder.incarnation_pubkey.as_bytes().as_slice())
            .bind(gen_i64)
            .execute(&self.pool)
            .await
            .map_err(|err| LeaseError::Backend(format!("{} release failed: {err}", DB::LABEL)))?;

        if DB::rows_affected(&res) == 1 {
            Ok(())
        } else {
            Err(self
                .classify_cas_failure(tenant, holder, expected_generation)
                .await)
        }
    }

    async fn current(&self, tenant: &TenantId) -> Result<Option<PlacementLease>, LeaseError> {
        let sql = DB::adapt_sql(
            "SELECT cell_role, placement_mode, holder_spiffe_id, holder_incarnation_pubkey,
                    holder_tls_exporter_proof, generation,
                    FLOOR(EXTRACT(EPOCH FROM expires_at) * 1000)::BIGINT AS expires_at_ms,
                    transfer_state, transfer_target_json
             FROM tenant_placement_leases
             WHERE tenant_id = $1
               AND transfer_state != 'released'
               AND expires_at > now()",
        );
        let row = sqlx::query::<DB>(&sql)
            .bind(tenant.as_str())
            .fetch_optional(&self.pool)
            .await
            .map_err(|err| LeaseError::Backend(format!("{} current failed: {err}", DB::LABEL)))?;

        row.as_ref()
            .map(|r| decode_lease_row::<DB>(tenant, r))
            .transpose()
    }
}

macro_rules! delegate_placement_lease_store {
    ($store:ty) => {
        impl $store {
            pub async fn mark_draining(
                &self,
                tenant: &TenantId,
                holder: &HolderInstance,
                expected_generation: FenceGeneration,
            ) -> Result<(), LeaseError> {
                self.core
                    .mark_draining(tenant, holder, expected_generation)
                    .await
            }
        }

        #[async_trait]
        impl PlacementLeaseStore for $store {
            async fn claim(
                &self,
                tenant: &TenantId,
                role: &CellRole,
                mode: PlacementMode,
                holder: &HolderInstance,
                ttl_ms: u64,
            ) -> Result<PlacementLease, LeaseError> {
                self.core.claim(tenant, role, mode, holder, ttl_ms).await
            }

            async fn renew(
                &self,
                tenant: &TenantId,
                holder: &HolderInstance,
                generation: FenceGeneration,
                ttl_ms: u64,
            ) -> Result<PlacementLease, LeaseError> {
                self.core.renew(tenant, holder, generation, ttl_ms).await
            }

            async fn transfer(
                &self,
                tenant: &TenantId,
                from_holder: &HolderInstance,
                to_holder: &HolderInstance,
                expected_generation: FenceGeneration,
                ttl_ms: u64,
            ) -> Result<PlacementLease, LeaseError> {
                self.core
                    .transfer(tenant, from_holder, to_holder, expected_generation, ttl_ms)
                    .await
            }

            async fn release(
                &self,
                tenant: &TenantId,
                holder: &HolderInstance,
                expected_generation: FenceGeneration,
            ) -> Result<(), LeaseError> {
                self.core.release(tenant, holder, expected_generation).await
            }

            async fn current(
                &self,
                tenant: &TenantId,
            ) -> Result<Option<PlacementLease>, LeaseError> {
                self.core.current(tenant).await
            }
        }
    };
}

// ============================================================================
// Production PostgreSQL placement store
// ============================================================================

/// Assert inside an open PostgreSQL transaction (`EnteredTransaction` /
/// `IngressUowTransaction`) that `scope` still holds the active placement lease
/// at `scope.placement_generation`, taking a `FOR SHARE` lock on the lease row.
pub async fn assert_placement_lease_for_share(
    tx: &mut Transaction<'_, Postgres>,
    scope: &TenantQueryScope,
) -> Result<(), TenantStoreError> {
    let gen_i64 = i64::try_from(scope.placement_generation.get())
        .map_err(|_| TenantStoreError::Backend("placement generation overflow".to_owned()))?;
    let row: Option<i32> = sqlx::query_scalar(ASSERT_PLACEMENT_LEASE_FOR_SHARE_SQL)
        .bind(scope.tenant_id.as_str())
        .bind(scope.holder.spiffe_id.as_str())
        .bind(scope.holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(gen_i64)
        .fetch_optional(&mut **tx)
        .await
        .map_err(|err| {
            TenantStoreError::Backend(format!("placement FOR SHARE check failed: {err}"))
        })?;
    check_write_fence_row(scope, row)
}

/// Production PostgreSQL [`PlacementLeaseStore`].
pub struct PostgresPlacementLeaseStore {
    core: PlacementLeaseCore<Postgres>,
}

impl PostgresPlacementLeaseStore {
    pub fn new(pool: PgPool) -> Self {
        Self {
            core: PlacementLeaseCore::new(pool),
        }
    }

    pub async fn ensure_schema(&self) -> Result<(), LeaseError> {
        sqlx::query(PG_PLACEMENT_SCHEMA)
            .execute(&self.core.pool)
            .await
            .map_err(|err| LeaseError::Backend(format!("pg placement DDL failed: {err}")))?;
        Ok(())
    }
}

delegate_placement_lease_store!(PostgresPlacementLeaseStore);

// ============================================================================
// In-process SQLite-compat placement harness for Postgres semantics
// ============================================================================

/// In-process SQLite-backed placement store mirroring [`PostgresPlacementLeaseStore`]
/// so all 7 lease/fencing tests run unconditionally on hosts without PostgreSQL.
pub struct SqliteCompatPlacementLeaseStore {
    core: PlacementLeaseCore<Sqlite>,
}

impl SqliteCompatPlacementLeaseStore {
    pub async fn new_in_memory() -> Result<Self, LeaseError> {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .map_err(|err| LeaseError::Backend(format!("sqlite connect: {err}")))?;
        let schema = sqlite_placement_schema();
        sqlx::query(&schema)
            .execute(&pool)
            .await
            .map_err(|err| LeaseError::Backend(format!("sqlite placement DDL: {err}")))?;
        Ok(Self {
            core: PlacementLeaseCore::new(pool),
        })
    }

    /// Mirrors `assert_placement_lease_for_share` inside the SQLite-compat harness.
    pub async fn assert_write_fence(
        &self,
        scope: &TenantQueryScope,
    ) -> Result<(), TenantStoreError> {
        let gen_i64 = i64::try_from(scope.placement_generation.get())
            .map_err(|_| TenantStoreError::Backend("generation overflow".to_owned()))?;
        let sql = adapt_pg_placement_sql_for_sqlite(ASSERT_PLACEMENT_LEASE_FOR_SHARE_SQL);
        let row: Option<i32> = sqlx::query_scalar(&sql)
            .bind(scope.tenant_id.as_str())
            .bind(scope.holder.spiffe_id.as_str())
            .bind(scope.holder.incarnation_pubkey.as_bytes().as_slice())
            .bind(gen_i64)
            .fetch_optional(&self.core.pool)
            .await
            .map_err(|err| TenantStoreError::Backend(format!("compat write fence: {err}")))?;
        check_write_fence_row(scope, row)
    }
}

delegate_placement_lease_store!(SqliteCompatPlacementLeaseStore);
