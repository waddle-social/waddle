//! PostgreSQL [`PlacementLeaseStore`] (`PostgresPlacementLeaseStore` using
//! `sqlx::PgPool`, PostgreSQL `now()`, and `FOR SHARE` write fencing) plus an
//! in-process SQLite-compat placement store (`SqliteCompatPlacementLeaseStore`)
//! so all 7 placement & fencing scenarios run unconditionally on hosts without
//! a local Postgres daemon.

use async_trait::async_trait;
use sqlx::{PgPool, Postgres, Row, SqlitePool, Transaction};
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

/// Production PostgreSQL [`PlacementLeaseStore`].
pub struct PostgresPlacementLeaseStore {
    pool: PgPool,
}

impl PostgresPlacementLeaseStore {
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    pub async fn ensure_schema(&self) -> Result<(), LeaseError> {
        sqlx::query(PG_PLACEMENT_SCHEMA)
            .execute(&self.pool)
            .await
            .map_err(|err| LeaseError::Backend(format!("pg placement DDL failed: {err}")))?;
        Ok(())
    }

    pub async fn mark_draining(
        &self,
        tenant: &TenantId,
        holder: &HolderInstance,
        expected_generation: FenceGeneration,
    ) -> Result<(), LeaseError> {
        let gen_i64 = i64::try_from(expected_generation.get())
            .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))?;
        let res = sqlx::query(
            "UPDATE tenant_placement_leases
             SET transfer_state = 'draining', updated_at = now()
             WHERE tenant_id = $1
               AND holder_spiffe_id = $2
               AND holder_incarnation_pubkey = $3
               AND generation = $4
               AND transfer_state IN ('active', 'draining')
               AND expires_at > now()",
        )
        .bind(tenant.as_str())
        .bind(holder.spiffe_id.as_str())
        .bind(holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(gen_i64)
        .execute(&self.pool)
        .await
        .map_err(|err| LeaseError::Backend(format!("pg mark_draining failed: {err}")))?;

        if res.rows_affected() == 1 {
            Ok(())
        } else {
            Err(self
                .classify_cas_failure(tenant, holder, expected_generation)
                .await)
        }
    }

    async fn classify_cas_failure(
        &self,
        tenant: &TenantId,
        holder: &HolderInstance,
        expected_generation: FenceGeneration,
    ) -> LeaseError {
        let row = sqlx::query(
            "SELECT holder_spiffe_id, holder_incarnation_pubkey, generation,
                    transfer_state, (expires_at <= now()) AS expired
             FROM tenant_placement_leases WHERE tenant_id = $1",
        )
        .bind(tenant.as_str())
        .fetch_optional(&self.pool)
        .await;

        let row = match row {
            Ok(Some(r)) => r,
            Ok(None) => return LeaseError::NotFound(tenant.clone()),
            Err(err) => return LeaseError::Backend(format!("pg classify failed: {err}")),
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
        LeaseError::Backend("pg placement CAS rejected".to_owned())
    }
}

fn validate_ttl(ttl_ms: u64) -> Result<f64, LeaseError> {
    if ttl_ms == 0 || ttl_ms > MAX_LEASE_TTL_MS {
        return Err(LeaseError::InvalidTtl(ttl_ms));
    }
    Ok((ttl_ms as f64) / 1000.0)
}

fn mode_to_str(mode: PlacementMode) -> &'static str {
    match mode {
        PlacementMode::SharedPool => "shared_pool",
        PlacementMode::DedicatedCell => "dedicated_cell",
    }
}

fn parse_pg_row(
    tenant: &TenantId,
    row: &sqlx::postgres::PgRow,
) -> Result<PlacementLease, LeaseError> {
    let cell_role_str: String = row.get("cell_role");
    let mode_str: String = row.get("placement_mode");
    let spiffe_str: String = row.get("holder_spiffe_id");
    let pk_vec: Vec<u8> = row.get("holder_incarnation_pubkey");
    let proof_vec: Vec<u8> = row.get("holder_tls_exporter_proof");
    let gen_i64: i64 = row.get("generation");
    let expires_at_ms: i64 = row.get("expires_at_ms");
    let state_str: String = row.get("transfer_state");
    let target_json: Option<serde_json::Value> = row.get("transfer_target_json");

    let cell_role = CellRole::parse(&cell_role_str)
        .map_err(|e| LeaseError::Backend(format!("invalid stored cell_role: {e}")))?;
    let mode = match mode_str.as_str() {
        "shared_pool" => PlacementMode::SharedPool,
        "dedicated_cell" => PlacementMode::DedicatedCell,
        other => {
            return Err(LeaseError::Backend(format!(
                "unknown placement_mode: {other}"
            )))
        }
    };
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
        "transferring" => {
            let val = target_json.ok_or_else(|| {
                LeaseError::Backend("transferring missing target json".to_owned())
            })?;
            let target: HolderInstance = serde_json::from_value(val)
                .map_err(|e| LeaseError::Backend(format!("invalid transfer target: {e}")))?;
            TransferState::Transferring { target }
        }
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

#[async_trait]
impl PlacementLeaseStore for PostgresPlacementLeaseStore {
    async fn claim(
        &self,
        tenant: &TenantId,
        role: &CellRole,
        mode: PlacementMode,
        holder: &HolderInstance,
        ttl_ms: u64,
    ) -> Result<PlacementLease, LeaseError> {
        let ttl_secs = validate_ttl(ttl_ms)?;
        if !spiffe_matches_cell_role(&holder.spiffe_id, role, mode) {
            return Err(LeaseError::HolderMismatch(tenant.clone()));
        }

        let row = sqlx::query(
            "INSERT INTO tenant_placement_leases
             (tenant_id, cell_role, placement_mode, holder_spiffe_id,
              holder_incarnation_pubkey, holder_tls_exporter_proof,
              generation, expires_at, transfer_state, transfer_target_json, updated_at)
             VALUES ($1, $2, $3, $4, $5, $6, 1, now() + make_interval(secs => $7), 'active', NULL, now())
             ON CONFLICT (tenant_id) DO UPDATE SET
                 cell_role = EXCLUDED.cell_role,
                 placement_mode = EXCLUDED.placement_mode,
                 holder_spiffe_id = EXCLUDED.holder_spiffe_id,
                 holder_incarnation_pubkey = EXCLUDED.holder_incarnation_pubkey,
                 holder_tls_exporter_proof = EXCLUDED.holder_tls_exporter_proof,
                 generation = tenant_placement_leases.generation + 1,
                 expires_at = now() + make_interval(secs => $7),
                 transfer_state = 'active',
                 transfer_target_json = NULL,
                 updated_at = now()
             WHERE tenant_placement_leases.transfer_state = 'released'
                OR tenant_placement_leases.expires_at <= now()
             RETURNING cell_role, placement_mode, holder_spiffe_id, holder_incarnation_pubkey,
                       holder_tls_exporter_proof, generation,
                       FLOOR(EXTRACT(EPOCH FROM expires_at) * 1000)::BIGINT AS expires_at_ms,
                       transfer_state, transfer_target_json",
        )
        .bind(tenant.as_str())
        .bind(role.as_str())
        .bind(mode_to_str(mode))
        .bind(holder.spiffe_id.as_str())
        .bind(holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(holder.tls_exporter_proof.as_bytes())
        .bind(ttl_secs)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LeaseError::Backend(format!("pg claim failed: {err}")))?;

        if let Some(row) = row {
            parse_pg_row(tenant, &row)
        } else {
            let gen_i64: i64 = sqlx::query_scalar(
                "SELECT generation FROM tenant_placement_leases WHERE tenant_id = $1",
            )
            .bind(tenant.as_str())
            .fetch_one(&self.pool)
            .await
            .map_err(|err| LeaseError::Backend(format!("pg read gen failed: {err}")))?;
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
        let ttl_secs = validate_ttl(ttl_ms)?;
        let gen_i64 = i64::try_from(generation.get())
            .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))?;
        let row = sqlx::query(
            "UPDATE tenant_placement_leases
             SET expires_at = now() + make_interval(secs => $5),
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
        )
        .bind(tenant.as_str())
        .bind(holder.spiffe_id.as_str())
        .bind(holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(gen_i64)
        .bind(ttl_secs)
        .bind(holder.tls_exporter_proof.as_bytes())
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LeaseError::Backend(format!("pg renew failed: {err}")))?;

        match row {
            Some(row) => parse_pg_row(tenant, &row),
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
        let ttl_secs = validate_ttl(ttl_ms)?;
        let gen_i64 = i64::try_from(expected_generation.get())
            .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))?;

        let meta = sqlx::query(
            "SELECT cell_role, placement_mode FROM tenant_placement_leases WHERE tenant_id = $1",
        )
        .bind(tenant.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LeaseError::Backend(format!("pg transfer meta: {err}")))?;
        let Some(meta) = meta else {
            return Err(LeaseError::NotFound(tenant.clone()));
        };
        let role_str: String = meta.get(0);
        let mode_str: String = meta.get(1);
        let role = CellRole::parse(&role_str)
            .map_err(|e| LeaseError::Backend(format!("invalid cell_role: {e}")))?;
        let mode = match mode_str.as_str() {
            "shared_pool" => PlacementMode::SharedPool,
            "dedicated_cell" => PlacementMode::DedicatedCell,
            other => return Err(LeaseError::Backend(format!("unknown mode: {other}"))),
        };
        if !spiffe_matches_cell_role(&to_holder.spiffe_id, &role, mode) {
            return Err(LeaseError::HolderMismatch(tenant.clone()));
        }

        let row = sqlx::query(
            "UPDATE tenant_placement_leases
             SET holder_spiffe_id = $5,
                 holder_incarnation_pubkey = $6,
                 holder_tls_exporter_proof = $7,
                 generation = generation + 1,
                 expires_at = now() + make_interval(secs => $8),
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
        )
        .bind(tenant.as_str())
        .bind(from_holder.spiffe_id.as_str())
        .bind(from_holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(gen_i64)
        .bind(to_holder.spiffe_id.as_str())
        .bind(to_holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(to_holder.tls_exporter_proof.as_bytes())
        .bind(ttl_secs)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LeaseError::Backend(format!("pg transfer failed: {err}")))?;

        match row {
            Some(row) => parse_pg_row(tenant, &row),
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
        let gen_i64 = i64::try_from(expected_generation.get())
            .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))?;
        let res = sqlx::query(
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
        )
        .bind(tenant.as_str())
        .bind(holder.spiffe_id.as_str())
        .bind(holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(gen_i64)
        .execute(&self.pool)
        .await
        .map_err(|err| LeaseError::Backend(format!("pg release failed: {err}")))?;

        if res.rows_affected() == 1 {
            Ok(())
        } else {
            Err(self
                .classify_cas_failure(tenant, holder, expected_generation)
                .await)
        }
    }

    async fn current(&self, tenant: &TenantId) -> Result<Option<PlacementLease>, LeaseError> {
        let row = sqlx::query(
            "SELECT cell_role, placement_mode, holder_spiffe_id, holder_incarnation_pubkey,
                    holder_tls_exporter_proof, generation,
                    FLOOR(EXTRACT(EPOCH FROM expires_at) * 1000)::BIGINT AS expires_at_ms,
                    transfer_state, transfer_target_json
             FROM tenant_placement_leases
             WHERE tenant_id = $1
               AND transfer_state != 'released'
               AND expires_at > now()",
        )
        .bind(tenant.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LeaseError::Backend(format!("pg current failed: {err}")))?;

        row.map(|r| parse_pg_row(tenant, &r)).transpose()
    }
}

// ============================================================================
// In-process SQLite-compat placement harness for Postgres semantics
// ============================================================================

const SQLITE_COMPAT_PLACEMENT_SCHEMA: &str = r#"CREATE TABLE IF NOT EXISTS tenant_placement_leases (
    tenant_id                 TEXT PRIMARY KEY,
    cell_role                 TEXT NOT NULL,
    placement_mode            TEXT NOT NULL CHECK (placement_mode IN ('shared_pool', 'dedicated_cell')),
    holder_spiffe_id          TEXT NOT NULL,
    holder_incarnation_pubkey BLOB NOT NULL CHECK (length(holder_incarnation_pubkey) = 32),
    holder_tls_exporter_proof BLOB NOT NULL CHECK (length(holder_tls_exporter_proof) BETWEEN 1 AND 128),
    generation                INTEGER NOT NULL CHECK (generation >= 1),
    expires_at_ms             INTEGER NOT NULL,
    transfer_state            TEXT NOT NULL CHECK (transfer_state IN ('active', 'transferring', 'draining', 'released')),
    transfer_target_json      TEXT,
    updated_at_ms             INTEGER NOT NULL
)"#;

/// In-process SQLite-backed placement store mirroring [`PostgresPlacementLeaseStore`]
/// so all 7 lease/fencing tests run unconditionally on hosts without PostgreSQL.
pub struct SqliteCompatPlacementLeaseStore {
    pool: SqlitePool,
    /// Serializes SQLite write CAS transactions (mirroring `BEGIN IMMEDIATE`
    /// so concurrent `BEGIN DEFERRED` readers never deadlock on write upgrade).
    write_lock: tokio::sync::Mutex<()>,
}

impl SqliteCompatPlacementLeaseStore {
    pub async fn new_in_memory() -> Result<Self, LeaseError> {
        let pool = sqlx::sqlite::SqlitePoolOptions::new()
            .max_connections(1)
            .connect("sqlite::memory:")
            .await
            .map_err(|err| LeaseError::Backend(format!("sqlite connect: {err}")))?;
        sqlx::query(SQLITE_COMPAT_PLACEMENT_SCHEMA)
            .execute(&pool)
            .await
            .map_err(|err| LeaseError::Backend(format!("sqlite placement DDL: {err}")))?;
        Ok(Self {
            pool,
            write_lock: tokio::sync::Mutex::new(()),
        })
    }

    pub async fn mark_draining(
        &self,
        tenant: &TenantId,
        holder: &HolderInstance,
        expected_generation: FenceGeneration,
    ) -> Result<(), LeaseError> {
        let gen_i64 = i64::try_from(expected_generation.get())
            .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))?;
        let now_ms = self.db_now_ms().await?;
        let res = sqlx::query(
            "UPDATE tenant_placement_leases
             SET transfer_state = 'draining', updated_at_ms = ?1
             WHERE tenant_id = ?2
               AND holder_spiffe_id = ?3
               AND holder_incarnation_pubkey = ?4
               AND generation = ?5
               AND transfer_state IN ('active', 'draining')
               AND expires_at_ms > ?1",
        )
        .bind(now_ms)
        .bind(tenant.as_str())
        .bind(holder.spiffe_id.as_str())
        .bind(holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(gen_i64)
        .execute(&self.pool)
        .await
        .map_err(|err| LeaseError::Backend(format!("sqlite mark_draining: {err}")))?;
        if res.rows_affected() == 1 {
            Ok(())
        } else {
            Err(self
                .classify_cas_failure(tenant, holder, expected_generation, now_ms)
                .await)
        }
    }

    /// Mirrors `assert_placement_lease_for_share` inside the SQLite-compat harness.
    pub async fn assert_write_fence(
        &self,
        scope: &TenantQueryScope,
    ) -> Result<(), TenantStoreError> {
        let gen_i64 = i64::try_from(scope.placement_generation.get())
            .map_err(|_| TenantStoreError::Backend("generation overflow".to_owned()))?;
        let now_ms = self
            .db_now_ms()
            .await
            .map_err(|err| TenantStoreError::Backend(err.to_string()))?;
        let row: Option<i32> = sqlx::query_scalar(
            "SELECT 1 FROM tenant_placement_leases
             WHERE tenant_id = ?1
               AND holder_spiffe_id = ?2
               AND holder_incarnation_pubkey = ?3
               AND generation = ?4
               AND transfer_state IN ('active', 'draining')
               AND expires_at_ms > ?5",
        )
        .bind(scope.tenant_id.as_str())
        .bind(scope.holder.spiffe_id.as_str())
        .bind(scope.holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(gen_i64)
        .bind(now_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| TenantStoreError::Backend(format!("compat write fence: {err}")))?;

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

    async fn db_now_ms(&self) -> Result<i64, LeaseError> {
        sqlx::query_scalar("SELECT CAST((julianday('now') - 2440587.5) * 86400000.0 AS INTEGER)")
            .fetch_one(&self.pool)
            .await
            .map_err(|err| LeaseError::Backend(format!("sqlite clock: {err}")))
    }

    async fn classify_cas_failure(
        &self,
        tenant: &TenantId,
        holder: &HolderInstance,
        expected_generation: FenceGeneration,
        now_ms: i64,
    ) -> LeaseError {
        let row = sqlx::query(
            "SELECT holder_spiffe_id, holder_incarnation_pubkey, generation,
                    transfer_state, expires_at_ms
             FROM tenant_placement_leases WHERE tenant_id = ?1",
        )
        .bind(tenant.as_str())
        .fetch_optional(&self.pool)
        .await;
        let row = match row {
            Ok(Some(r)) => r,
            Ok(None) => return LeaseError::NotFound(tenant.clone()),
            Err(err) => return LeaseError::Backend(format!("classify: {err}")),
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
        let expires_at_ms: i64 = row.get(4);
        if state == "released" || expires_at_ms <= now_ms {
            return LeaseError::LeaseExpired(tenant.clone());
        }
        LeaseError::Backend("compat placement CAS rejected".to_owned())
    }

    fn parse_sqlite_row(
        tenant: &TenantId,
        row: &sqlx::sqlite::SqliteRow,
    ) -> Result<PlacementLease, LeaseError> {
        let cell_role_str: String = row.get("cell_role");
        let mode_str: String = row.get("placement_mode");
        let spiffe_str: String = row.get("holder_spiffe_id");
        let pk_vec: Vec<u8> = row.get("holder_incarnation_pubkey");
        let proof_vec: Vec<u8> = row.get("holder_tls_exporter_proof");
        let gen_i64: i64 = row.get("generation");
        let expires_at_ms: i64 = row.get("expires_at_ms");
        let state_str: String = row.get("transfer_state");
        let target_json: Option<String> = row.get("transfer_target_json");

        let cell_role = CellRole::parse(&cell_role_str)
            .map_err(|e| LeaseError::Backend(format!("invalid cell_role: {e}")))?;
        let mode = match mode_str.as_str() {
            "shared_pool" => PlacementMode::SharedPool,
            "dedicated_cell" => PlacementMode::DedicatedCell,
            other => return Err(LeaseError::Backend(format!("unknown mode: {other}"))),
        };
        let spiffe_id = SpiffeId::parse(&spiffe_str)
            .map_err(|e| LeaseError::Backend(format!("invalid spiffe_id: {e}")))?;
        let pk: [u8; 32] = pk_vec
            .try_into()
            .map_err(|_| LeaseError::Backend("pubkey != 32 bytes".to_owned()))?;
        let proof = TlsExporterProof::new(proof_vec)
            .map_err(|e| LeaseError::Backend(format!("invalid proof: {e}")))?;
        let transfer_state = match state_str.as_str() {
            "active" => TransferState::Active,
            "draining" => TransferState::Draining,
            "transferring" => {
                let raw = target_json
                    .ok_or_else(|| LeaseError::Backend("missing target_json".to_owned()))?;
                let target: HolderInstance = serde_json::from_str(&raw)
                    .map_err(|e| LeaseError::Backend(format!("invalid target_json: {e}")))?;
                TransferState::Transferring { target }
            }
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
}

#[async_trait]
impl PlacementLeaseStore for SqliteCompatPlacementLeaseStore {
    async fn claim(
        &self,
        tenant: &TenantId,
        role: &CellRole,
        mode: PlacementMode,
        holder: &HolderInstance,
        ttl_ms: u64,
    ) -> Result<PlacementLease, LeaseError> {
        let _ = validate_ttl(ttl_ms)?;
        let ttl_i64 = i64::try_from(ttl_ms).map_err(|_| LeaseError::InvalidTtl(ttl_ms))?;
        if !spiffe_matches_cell_role(&holder.spiffe_id, role, mode) {
            return Err(LeaseError::HolderMismatch(tenant.clone()));
        }
        let _guard = self.write_lock.lock().await;
        let now_ms = self.db_now_ms().await?;
        let expires_at_ms = now_ms.saturating_add(ttl_i64);

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(|err| LeaseError::Backend(format!("begin tx: {err}")))?;
        let existing = sqlx::query(
            "SELECT generation, transfer_state, expires_at_ms
             FROM tenant_placement_leases WHERE tenant_id = ?1",
        )
        .bind(tenant.as_str())
        .fetch_optional(&mut *tx)
        .await
        .map_err(|err| LeaseError::Backend(format!("read lease: {err}")))?;

        let new_gen = match existing {
            None => {
                sqlx::query(
                    "INSERT INTO tenant_placement_leases
                     (tenant_id, cell_role, placement_mode, holder_spiffe_id,
                      holder_incarnation_pubkey, holder_tls_exporter_proof,
                      generation, expires_at_ms, transfer_state, transfer_target_json, updated_at_ms)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, 'active', NULL, ?8)",
                )
                .bind(tenant.as_str())
                .bind(role.as_str())
                .bind(mode_to_str(mode))
                .bind(holder.spiffe_id.as_str())
                .bind(holder.incarnation_pubkey.as_bytes().as_slice())
                .bind(holder.tls_exporter_proof.as_bytes())
                .bind(expires_at_ms)
                .bind(now_ms)
                .execute(&mut *tx)
                .await
                .map_err(|err| LeaseError::Backend(format!("insert lease: {err}")))?;
                FenceGeneration::new(1)
            }
            Some(row) => {
                let prev_gen_i64: i64 = row.get(0);
                let prev_gen = FenceGeneration::new(u64::try_from(prev_gen_i64).unwrap_or(0));
                let state: String = row.get(1);
                let prev_exp: i64 = row.get(2);
                if state != "released" && prev_exp > now_ms {
                    return Err(LeaseError::AlreadyHeld {
                        tenant_id: tenant.clone(),
                        current_generation: prev_gen,
                    });
                }
                let next_gen = prev_gen
                    .checked_next()
                    .ok_or_else(|| LeaseError::Backend("generation overflow".to_owned()))?;
                let next_gen_i64 = i64::try_from(next_gen.get())
                    .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))?;
                let res = sqlx::query(
                    "UPDATE tenant_placement_leases
                     SET cell_role = ?2,
                         placement_mode = ?3,
                         holder_spiffe_id = ?4,
                         holder_incarnation_pubkey = ?5,
                         holder_tls_exporter_proof = ?6,
                         generation = ?7,
                         expires_at_ms = ?8,
                         transfer_state = 'active',
                         transfer_target_json = NULL,
                         updated_at_ms = ?9
                     WHERE tenant_id = ?1
                       AND generation = ?10
                       AND (transfer_state = 'released' OR expires_at_ms <= ?9)",
                )
                .bind(tenant.as_str())
                .bind(role.as_str())
                .bind(mode_to_str(mode))
                .bind(holder.spiffe_id.as_str())
                .bind(holder.incarnation_pubkey.as_bytes().as_slice())
                .bind(holder.tls_exporter_proof.as_bytes())
                .bind(next_gen_i64)
                .bind(expires_at_ms)
                .bind(now_ms)
                .bind(prev_gen_i64)
                .execute(&mut *tx)
                .await
                .map_err(|err| LeaseError::Backend(format!("reclaim lease: {err}")))?;
                if res.rows_affected() != 1 {
                    return Err(LeaseError::AlreadyHeld {
                        tenant_id: tenant.clone(),
                        current_generation: prev_gen,
                    });
                }
                next_gen
            }
        };

        tx.commit()
            .await
            .map_err(|err| LeaseError::Backend(format!("commit claim: {err}")))?;

        Ok(PlacementLease {
            tenant_id: tenant.clone(),
            cell_role: role.clone(),
            mode,
            holder: holder.clone(),
            generation: new_gen,
            expires_at_ms,
            transfer_state: TransferState::Active,
        })
    }

    async fn renew(
        &self,
        tenant: &TenantId,
        holder: &HolderInstance,
        generation: FenceGeneration,
        ttl_ms: u64,
    ) -> Result<PlacementLease, LeaseError> {
        let _ = validate_ttl(ttl_ms)?;
        let ttl_i64 = i64::try_from(ttl_ms).map_err(|_| LeaseError::InvalidTtl(ttl_ms))?;
        let gen_i64 = i64::try_from(generation.get())
            .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))?;
        let now_ms = self.db_now_ms().await?;
        let new_expires = now_ms.saturating_add(ttl_i64);

        let res = sqlx::query(
            "UPDATE tenant_placement_leases
             SET expires_at_ms = ?1,
                 holder_tls_exporter_proof = ?2,
                 updated_at_ms = ?3
             WHERE tenant_id = ?4
               AND holder_spiffe_id = ?5
               AND holder_incarnation_pubkey = ?6
               AND generation = ?7
               AND transfer_state IN ('active', 'draining', 'transferring')
               AND expires_at_ms > ?3",
        )
        .bind(new_expires)
        .bind(holder.tls_exporter_proof.as_bytes())
        .bind(now_ms)
        .bind(tenant.as_str())
        .bind(holder.spiffe_id.as_str())
        .bind(holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(gen_i64)
        .execute(&self.pool)
        .await
        .map_err(|err| LeaseError::Backend(format!("compat renew: {err}")))?;

        if res.rows_affected() == 1 {
            let row = sqlx::query(
                "SELECT cell_role, placement_mode, holder_spiffe_id, holder_incarnation_pubkey,
                        holder_tls_exporter_proof, generation, expires_at_ms, transfer_state,
                        transfer_target_json
                 FROM tenant_placement_leases WHERE tenant_id = ?1",
            )
            .bind(tenant.as_str())
            .fetch_one(&self.pool)
            .await
            .map_err(|err| LeaseError::Backend(format!("compat renew fetch: {err}")))?;
            Self::parse_sqlite_row(tenant, &row)
        } else {
            Err(self
                .classify_cas_failure(tenant, holder, generation, now_ms)
                .await)
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
        let _ = validate_ttl(ttl_ms)?;
        let ttl_i64 = i64::try_from(ttl_ms).map_err(|_| LeaseError::InvalidTtl(ttl_ms))?;
        let gen_i64 = i64::try_from(expected_generation.get())
            .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))?;
        let now_ms = self.db_now_ms().await?;
        let new_expires = now_ms.saturating_add(ttl_i64);

        let meta = sqlx::query(
            "SELECT cell_role, placement_mode FROM tenant_placement_leases WHERE tenant_id = ?1",
        )
        .bind(tenant.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LeaseError::Backend(format!("compat transfer meta: {err}")))?;
        let Some(meta) = meta else {
            return Err(LeaseError::NotFound(tenant.clone()));
        };
        let role_str: String = meta.get(0);
        let mode_str: String = meta.get(1);
        let role = CellRole::parse(&role_str)
            .map_err(|e| LeaseError::Backend(format!("invalid cell_role: {e}")))?;
        let mode = match mode_str.as_str() {
            "shared_pool" => PlacementMode::SharedPool,
            "dedicated_cell" => PlacementMode::DedicatedCell,
            other => return Err(LeaseError::Backend(format!("unknown mode: {other}"))),
        };
        if !spiffe_matches_cell_role(&to_holder.spiffe_id, &role, mode) {
            return Err(LeaseError::HolderMismatch(tenant.clone()));
        }

        let res = sqlx::query(
            "UPDATE tenant_placement_leases
             SET holder_spiffe_id = ?1,
                 holder_incarnation_pubkey = ?2,
                 holder_tls_exporter_proof = ?3,
                 generation = generation + 1,
                 expires_at_ms = ?4,
                 transfer_state = 'active',
                 transfer_target_json = NULL,
                 updated_at_ms = ?5
             WHERE tenant_id = ?6
               AND holder_spiffe_id = ?7
               AND holder_incarnation_pubkey = ?8
               AND generation = ?9
               AND transfer_state != 'released'
               AND expires_at_ms > ?5",
        )
        .bind(to_holder.spiffe_id.as_str())
        .bind(to_holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(to_holder.tls_exporter_proof.as_bytes())
        .bind(new_expires)
        .bind(now_ms)
        .bind(tenant.as_str())
        .bind(from_holder.spiffe_id.as_str())
        .bind(from_holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(gen_i64)
        .execute(&self.pool)
        .await
        .map_err(|err| LeaseError::Backend(format!("compat transfer: {err}")))?;

        if res.rows_affected() == 1 {
            let row = sqlx::query(
                "SELECT cell_role, placement_mode, holder_spiffe_id, holder_incarnation_pubkey,
                        holder_tls_exporter_proof, generation, expires_at_ms, transfer_state,
                        transfer_target_json
                 FROM tenant_placement_leases WHERE tenant_id = ?1",
            )
            .bind(tenant.as_str())
            .fetch_one(&self.pool)
            .await
            .map_err(|err| LeaseError::Backend(format!("compat transfer fetch: {err}")))?;
            Self::parse_sqlite_row(tenant, &row)
        } else {
            Err(self
                .classify_cas_failure(tenant, from_holder, expected_generation, now_ms)
                .await)
        }
    }

    async fn release(
        &self,
        tenant: &TenantId,
        holder: &HolderInstance,
        expected_generation: FenceGeneration,
    ) -> Result<(), LeaseError> {
        let gen_i64 = i64::try_from(expected_generation.get())
            .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))?;
        let now_ms = self.db_now_ms().await?;

        let res = sqlx::query(
            "UPDATE tenant_placement_leases
             SET transfer_state = 'released',
                 expires_at_ms = 0,
                 transfer_target_json = NULL,
                 updated_at_ms = ?1
             WHERE tenant_id = ?2
               AND holder_spiffe_id = ?3
               AND holder_incarnation_pubkey = ?4
               AND generation = ?5
               AND transfer_state != 'released'",
        )
        .bind(now_ms)
        .bind(tenant.as_str())
        .bind(holder.spiffe_id.as_str())
        .bind(holder.incarnation_pubkey.as_bytes().as_slice())
        .bind(gen_i64)
        .execute(&self.pool)
        .await
        .map_err(|err| LeaseError::Backend(format!("compat release: {err}")))?;

        if res.rows_affected() == 1 {
            Ok(())
        } else {
            Err(self
                .classify_cas_failure(tenant, holder, expected_generation, now_ms)
                .await)
        }
    }

    async fn current(&self, tenant: &TenantId) -> Result<Option<PlacementLease>, LeaseError> {
        let now_ms = self.db_now_ms().await?;
        let row = sqlx::query(
            "SELECT cell_role, placement_mode, holder_spiffe_id, holder_incarnation_pubkey,
                    holder_tls_exporter_proof, generation, expires_at_ms, transfer_state,
                    transfer_target_json
             FROM tenant_placement_leases
             WHERE tenant_id = ?1
               AND transfer_state != 'released'
               AND expires_at_ms > ?2",
        )
        .bind(tenant.as_str())
        .bind(now_ms)
        .fetch_optional(&self.pool)
        .await
        .map_err(|err| LeaseError::Backend(format!("compat current: {err}")))?;

        row.map(|r| Self::parse_sqlite_row(tenant, &r)).transpose()
    }
}
