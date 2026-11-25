//! Turso control-plane [`PlacementLeaseStore`] (`placement.db` in WAL mode with
//! `PRAGMA synchronous = FULL`) using durable `BEGIN IMMEDIATE` CAS and the
//! primary storage host's authoritative `CAST(unixepoch('subsec') * 1000 AS INTEGER)`.

use std::path::Path;

use async_trait::async_trait;
use tokio::sync::Mutex;
use waddle_broker_core::identity::{
    CellRole, HolderInstance, IncarnationPubKey, SpiffeId, TenantId, TlsExporterProof,
};
use waddle_broker_core::placement::{
    FenceGeneration, LeaseError, PlacementLease, PlacementLeaseStore, PlacementMode, TransferState,
};
use waddle_broker_core::store::{TenantQueryScope, TenantStoreError};

use crate::spiffe_x509::spiffe_matches_cell_role;

/// Maximum permitted lease TTL (5 minutes).
pub const MAX_LEASE_TTL_MS: u64 = 300_000;

const PLACEMENT_SCHEMA: &str = r#"CREATE TABLE IF NOT EXISTS placement_leases (
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

/// Turso WAL-mode control-plane [`PlacementLeaseStore`].
pub struct TursoPlacementLeaseStore {
    _db: turso::Database,
    /// Serializes `BEGIN IMMEDIATE` transactions on the control-plane connection.
    conn: Mutex<turso::Connection>,
}

impl TursoPlacementLeaseStore {
    pub async fn open(path: &Path) -> Result<Self, LeaseError> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                LeaseError::Backend(format!("create placement dir failed: {err}"))
            })?;
        }
        let path_str = path
            .to_str()
            .ok_or_else(|| LeaseError::Backend("non-UTF-8 placement DB path".to_owned()))?;
        let db = turso::Builder::new_local(path_str)
            .build()
            .await
            .map_err(|err| LeaseError::Backend(format!("turso placement open failed: {err}")))?;
        let conn = db
            .connect()
            .map_err(|err| LeaseError::Backend(format!("turso placement connect failed: {err}")))?;
        let _ = conn
            .pragma_update("journal_mode", "WAL")
            .await
            .map_err(|err| LeaseError::Backend(format!("WAL pragma failed: {err}")))?;
        let _ = conn
            .pragma_update("synchronous", "FULL")
            .await
            .map_err(|err| LeaseError::Backend(format!("synchronous=FULL pragma failed: {err}")))?;
        let _ = conn
            .pragma_update("foreign_keys", "ON")
            .await
            .map_err(|err| LeaseError::Backend(format!("foreign_keys pragma failed: {err}")))?;
        conn.execute(PLACEMENT_SCHEMA, ())
            .await
            .map_err(|err| LeaseError::Backend(format!("placement DDL failed: {err}")))?;
        Ok(Self {
            _db: db,
            conn: Mutex::new(conn),
        })
    }

    /// Mark an active lease as `draining` prior to graceful quiesce and handoff.
    pub async fn mark_draining(
        &self,
        tenant: &TenantId,
        holder: &HolderInstance,
        expected_generation: FenceGeneration,
    ) -> Result<(), LeaseError> {
        let gen_i64 = i64::try_from(expected_generation.get())
            .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))?;
        let conn = self.conn.lock().await;
        conn.execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|err| LeaseError::Backend(format!("BEGIN IMMEDIATE: {err}")))?;

        let result: Result<(), LeaseError> = async {
            let affected = conn
                .execute(
                    "UPDATE placement_leases
                     SET transfer_state = 'draining',
                         updated_at_ms = CAST(unixepoch('subsec') * 1000 AS INTEGER)
                     WHERE tenant_id = ?1
                       AND holder_spiffe_id = ?2
                       AND holder_incarnation_pubkey = ?3
                       AND generation = ?4
                       AND transfer_state IN ('active', 'draining')
                       AND expires_at_ms > CAST(unixepoch('subsec') * 1000 AS INTEGER)",
                    (
                        tenant.as_str(),
                        holder.spiffe_id.as_str(),
                        holder.incarnation_pubkey.as_bytes().to_vec(),
                        gen_i64,
                    ),
                )
                .await
                .map_err(|err| LeaseError::Backend(format!("mark_draining update: {err}")))?;
            if affected == 1 {
                Ok(())
            } else {
                Err(Self::classify_cas_failure(&conn, tenant, holder, expected_generation).await)
            }
        }
        .await;

        match result {
            Ok(()) => {
                if let Err(err) = conn.execute("COMMIT", ()).await {
                    let _ = conn.execute("ROLLBACK", ()).await;
                    return Err(LeaseError::Backend(format!("COMMIT: {err}")));
                }
                Ok(())
            }
            Err(err) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(err)
            }
        }
    }

    /// Mark an active/draining lease as `transferring` to `target` prior to atomic `transfer`.
    pub async fn begin_transfer(
        &self,
        tenant: &TenantId,
        from_holder: &HolderInstance,
        target: &HolderInstance,
        expected_generation: FenceGeneration,
    ) -> Result<(), LeaseError> {
        let target_json = serde_json::to_string(target)
            .map_err(|err| LeaseError::Backend(format!("serialize transfer target: {err}")))?;
        let gen_i64 = i64::try_from(expected_generation.get())
            .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))?;
        let conn = self.conn.lock().await;
        conn.execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|err| LeaseError::Backend(format!("BEGIN IMMEDIATE: {err}")))?;

        let result: Result<(), LeaseError> = async {
            let affected = conn
                .execute(
                    "UPDATE placement_leases
                     SET transfer_state = 'transferring',
                         transfer_target_json = ?5,
                         updated_at_ms = CAST(unixepoch('subsec') * 1000 AS INTEGER)
                     WHERE tenant_id = ?1
                       AND holder_spiffe_id = ?2
                       AND holder_incarnation_pubkey = ?3
                       AND generation = ?4
                       AND transfer_state IN ('active', 'draining', 'transferring')
                       AND expires_at_ms > CAST(unixepoch('subsec') * 1000 AS INTEGER)",
                    (
                        tenant.as_str(),
                        from_holder.spiffe_id.as_str(),
                        from_holder.incarnation_pubkey.as_bytes().to_vec(),
                        gen_i64,
                        target_json,
                    ),
                )
                .await
                .map_err(|err| LeaseError::Backend(format!("begin_transfer update: {err}")))?;
            if affected == 1 {
                Ok(())
            } else {
                Err(
                    Self::classify_cas_failure(&conn, tenant, from_holder, expected_generation)
                        .await,
                )
            }
        }
        .await;

        match result {
            Ok(()) => {
                if let Err(err) = conn.execute("COMMIT", ()).await {
                    let _ = conn.execute("ROLLBACK", ()).await;
                    return Err(LeaseError::Backend(format!("COMMIT: {err}")));
                }
                Ok(())
            }
            Err(err) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(err)
            }
        }
    }

    async fn db_now_ms(conn: &turso::Connection) -> Result<i64, LeaseError> {
        let mut rows = conn
            .query("SELECT CAST(unixepoch('subsec') * 1000 AS INTEGER)", ())
            .await
            .map_err(|err| LeaseError::Backend(format!("db clock query failed: {err}")))?;
        let row = rows
            .next()
            .await
            .map_err(|err| LeaseError::Backend(format!("db clock row failed: {err}")))?
            .ok_or_else(|| LeaseError::Backend("db clock returned no rows".to_owned()))?;
        row.get(0)
            .map_err(|err| LeaseError::Backend(format!("db clock col0: {err}")))
    }

    async fn read_raw_row(
        conn: &turso::Connection,
        tenant: &TenantId,
    ) -> Result<Option<RawLeaseRow>, LeaseError> {
        let mut rows = conn
            .query(
                "SELECT cell_role, placement_mode, holder_spiffe_id, holder_incarnation_pubkey,
                        holder_tls_exporter_proof, generation, expires_at_ms, transfer_state,
                        transfer_target_json
                 FROM placement_leases WHERE tenant_id = ?1",
                [tenant.as_str()],
            )
            .await
            .map_err(|err| LeaseError::Backend(format!("read lease failed: {err}")))?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|err| LeaseError::Backend(format!("read lease row: {err}")))?
        else {
            return Ok(None);
        };
        Ok(Some(RawLeaseRow {
            cell_role: row.get(0).map_err(|e| LeaseError::Backend(e.to_string()))?,
            placement_mode: row.get(1).map_err(|e| LeaseError::Backend(e.to_string()))?,
            holder_spiffe_id: row.get(2).map_err(|e| LeaseError::Backend(e.to_string()))?,
            holder_incarnation_pubkey: row
                .get(3)
                .map_err(|e| LeaseError::Backend(e.to_string()))?,
            holder_tls_exporter_proof: row
                .get(4)
                .map_err(|e| LeaseError::Backend(e.to_string()))?,
            generation: row.get(5).map_err(|e| LeaseError::Backend(e.to_string()))?,
            expires_at_ms: row.get(6).map_err(|e| LeaseError::Backend(e.to_string()))?,
            transfer_state: row.get(7).map_err(|e| LeaseError::Backend(e.to_string()))?,
            transfer_target_json: row.get(8).map_err(|e| LeaseError::Backend(e.to_string()))?,
        }))
    }

    async fn classify_cas_failure(
        conn: &turso::Connection,
        tenant: &TenantId,
        holder: &HolderInstance,
        expected_generation: FenceGeneration,
    ) -> LeaseError {
        let now_ms = match Self::db_now_ms(conn).await {
            Ok(ms) => ms,
            Err(err) => return err,
        };
        let raw = match Self::read_raw_row(conn, tenant).await {
            Ok(Some(row)) => row,
            Ok(None) => return LeaseError::NotFound(tenant.clone()),
            Err(err) => return err,
        };
        let actual_gen = FenceGeneration::new(u64::try_from(raw.generation).unwrap_or(0));
        if actual_gen != expected_generation {
            return LeaseError::StaleGeneration {
                tenant_id: tenant.clone(),
                expected: expected_generation,
                actual: actual_gen,
            };
        }
        if raw.holder_spiffe_id != holder.spiffe_id.as_str()
            || raw.holder_incarnation_pubkey.as_slice() != holder.incarnation_pubkey.as_bytes()
        {
            return LeaseError::HolderMismatch(tenant.clone());
        }
        if raw.transfer_state == "released" || raw.expires_at_ms <= now_ms {
            return LeaseError::LeaseExpired(tenant.clone());
        }
        LeaseError::Backend("placement CAS rejected in unexpected state".to_owned())
    }
}

struct RawLeaseRow {
    cell_role: String,
    placement_mode: String,
    holder_spiffe_id: String,
    holder_incarnation_pubkey: Vec<u8>,
    holder_tls_exporter_proof: Vec<u8>,
    generation: i64,
    expires_at_ms: i64,
    transfer_state: String,
    transfer_target_json: Option<String>,
}

impl RawLeaseRow {
    fn into_lease(self, tenant: &TenantId) -> Result<PlacementLease, LeaseError> {
        let cell_role = CellRole::parse(&self.cell_role)
            .map_err(|e| LeaseError::Backend(format!("stored cell_role invalid: {e}")))?;
        let mode = match self.placement_mode.as_str() {
            "shared_pool" => PlacementMode::SharedPool,
            "dedicated_cell" => PlacementMode::DedicatedCell,
            other => {
                return Err(LeaseError::Backend(format!(
                    "unknown placement_mode: {other}"
                )))
            }
        };
        let spiffe_id = SpiffeId::parse(&self.holder_spiffe_id)
            .map_err(|e| LeaseError::Backend(format!("stored spiffe_id invalid: {e}")))?;
        let pk_bytes: [u8; 32] = self
            .holder_incarnation_pubkey
            .try_into()
            .map_err(|_| LeaseError::Backend("stored incarnation_pubkey != 32 bytes".to_owned()))?;
        let proof = TlsExporterProof::new(self.holder_tls_exporter_proof)
            .map_err(|e| LeaseError::Backend(format!("stored proof invalid: {e}")))?;
        let transfer_state = match self.transfer_state.as_str() {
            "active" => TransferState::Active,
            "draining" => TransferState::Draining,
            "transferring" => {
                let json = self.transfer_target_json.ok_or_else(|| {
                    LeaseError::Backend(
                        "transferring state missing transfer_target_json".to_owned(),
                    )
                })?;
                let target: HolderInstance = serde_json::from_str(&json).map_err(|e| {
                    LeaseError::Backend(format!("invalid transfer target json: {e}"))
                })?;
                TransferState::Transferring { target }
            }
            other => {
                return Err(LeaseError::Backend(format!(
                    "unexpected active transfer_state: {other}"
                )))
            }
        };
        let gen = u64::try_from(self.generation)
            .map_err(|_| LeaseError::Backend("negative generation".to_owned()))?;
        Ok(PlacementLease {
            tenant_id: tenant.clone(),
            cell_role,
            mode,
            holder: HolderInstance {
                spiffe_id,
                incarnation_pubkey: IncarnationPubKey::from_bytes(pk_bytes),
                tls_exporter_proof: proof,
            },
            generation: FenceGeneration::new(gen),
            expires_at_ms: self.expires_at_ms,
            transfer_state,
        })
    }
}

fn validate_ttl(ttl_ms: u64) -> Result<i64, LeaseError> {
    if ttl_ms == 0 || ttl_ms > MAX_LEASE_TTL_MS {
        return Err(LeaseError::InvalidTtl(ttl_ms));
    }
    i64::try_from(ttl_ms).map_err(|_| LeaseError::InvalidTtl(ttl_ms))
}

fn mode_to_str(mode: PlacementMode) -> &'static str {
    match mode {
        PlacementMode::SharedPool => "shared_pool",
        PlacementMode::DedicatedCell => "dedicated_cell",
    }
}

#[async_trait]
impl PlacementLeaseStore for TursoPlacementLeaseStore {
    async fn claim(
        &self,
        tenant: &TenantId,
        role: &CellRole,
        mode: PlacementMode,
        holder: &HolderInstance,
        ttl_ms: u64,
    ) -> Result<PlacementLease, LeaseError> {
        let ttl_i64 = validate_ttl(ttl_ms)?;
        if !spiffe_matches_cell_role(&holder.spiffe_id, role, mode) {
            return Err(LeaseError::HolderMismatch(tenant.clone()));
        }

        let conn = self.conn.lock().await;
        conn.execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|err| LeaseError::Backend(format!("BEGIN IMMEDIATE: {err}")))?;

        let result: Result<PlacementLease, LeaseError> = async {
            let now_ms = Self::db_now_ms(&conn).await?;
            let expires_at_ms = now_ms.saturating_add(ttl_i64);
            let existing = Self::read_raw_row(&conn, tenant).await?;

            let new_gen = match existing {
                None => {
                    conn.execute(
                        "INSERT INTO placement_leases
                         (tenant_id, cell_role, placement_mode, holder_spiffe_id,
                          holder_incarnation_pubkey, holder_tls_exporter_proof,
                          generation, expires_at_ms, transfer_state, transfer_target_json, updated_at_ms)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 1, ?7, 'active', NULL, ?8)",
                        (
                            tenant.as_str(),
                            role.as_str(),
                            mode_to_str(mode),
                            holder.spiffe_id.as_str(),
                            holder.incarnation_pubkey.as_bytes().to_vec(),
                            holder.tls_exporter_proof.as_bytes().to_vec(),
                            expires_at_ms,
                            now_ms,
                        ),
                    )
                    .await
                    .map_err(|err| LeaseError::Backend(format!("claim insert failed: {err}")))?;
                    FenceGeneration::new(1)
                }
                Some(row) => {
                    let current_gen =
                        FenceGeneration::new(u64::try_from(row.generation).unwrap_or(0));
                    if row.transfer_state != "released" && row.expires_at_ms > now_ms {
                        return Err(LeaseError::AlreadyHeld {
                            tenant_id: tenant.clone(),
                            current_generation: current_gen,
                        });
                    }
                    let next_gen = current_gen
                        .checked_next()
                        .ok_or_else(|| LeaseError::Backend("generation overflow".to_owned()))?;
                    let next_gen_i64 = i64::try_from(next_gen.get())
                        .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))?;
                    let affected = conn
                        .execute(
                            "UPDATE placement_leases
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
                            (
                                tenant.as_str(),
                                role.as_str(),
                                mode_to_str(mode),
                                holder.spiffe_id.as_str(),
                                holder.incarnation_pubkey.as_bytes().to_vec(),
                                holder.tls_exporter_proof.as_bytes().to_vec(),
                                next_gen_i64,
                                expires_at_ms,
                                now_ms,
                                row.generation,
                            ),
                        )
                        .await
                        .map_err(|err| {
                            LeaseError::Backend(format!("claim update failed: {err}"))
                        })?;
                    if affected == 1 {
                        next_gen
                    } else {
                        return Err(LeaseError::AlreadyHeld {
                            tenant_id: tenant.clone(),
                            current_generation: current_gen,
                        });
                    }
                }
            };

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
        .await;

        match result {
            Ok(lease) => {
                if let Err(err) = conn.execute("COMMIT", ()).await {
                    let _ = conn.execute("ROLLBACK", ()).await;
                    return Err(LeaseError::Backend(format!("claim COMMIT failed: {err}")));
                }
                Ok(lease)
            }
            Err(err) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(err)
            }
        }
    }

    async fn renew(
        &self,
        tenant: &TenantId,
        holder: &HolderInstance,
        generation: FenceGeneration,
        ttl_ms: u64,
    ) -> Result<PlacementLease, LeaseError> {
        let ttl_i64 = validate_ttl(ttl_ms)?;
        let gen_i64 = i64::try_from(generation.get())
            .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))?;
        let conn = self.conn.lock().await;
        conn.execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|err| LeaseError::Backend(format!("BEGIN IMMEDIATE: {err}")))?;

        let result: Result<RawLeaseRow, LeaseError> = async {
            let now_ms = Self::db_now_ms(&conn).await?;
            let new_expires = now_ms.saturating_add(ttl_i64);

            let affected = conn
                .execute(
                    "UPDATE placement_leases
                     SET expires_at_ms = ?1,
                         holder_tls_exporter_proof = ?2,
                         updated_at_ms = ?3
                     WHERE tenant_id = ?4
                       AND holder_spiffe_id = ?5
                       AND holder_incarnation_pubkey = ?6
                       AND generation = ?7
                       AND transfer_state IN ('active', 'draining', 'transferring')
                       AND expires_at_ms > ?3",
                    (
                        new_expires,
                        holder.tls_exporter_proof.as_bytes().to_vec(),
                        now_ms,
                        tenant.as_str(),
                        holder.spiffe_id.as_str(),
                        holder.incarnation_pubkey.as_bytes().to_vec(),
                        gen_i64,
                    ),
                )
                .await
                .map_err(|err| LeaseError::Backend(format!("renew update failed: {err}")))?;

            if affected == 1 {
                Self::read_raw_row(&conn, tenant)
                    .await?
                    .ok_or_else(|| LeaseError::Backend("renewed row disappeared".to_owned()))
            } else {
                Err(Self::classify_cas_failure(&conn, tenant, holder, generation).await)
            }
        }
        .await;

        match result {
            Ok(raw) => {
                if let Err(err) = conn.execute("COMMIT", ()).await {
                    let _ = conn.execute("ROLLBACK", ()).await;
                    return Err(LeaseError::Backend(format!("renew COMMIT failed: {err}")));
                }
                raw.into_lease(tenant)
            }
            Err(err) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(err)
            }
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
        let ttl_i64 = validate_ttl(ttl_ms)?;
        let gen_i64 = i64::try_from(expected_generation.get())
            .map_err(|_| LeaseError::Backend("generation overflow".to_owned()))?;
        let conn = self.conn.lock().await;
        conn.execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|err| LeaseError::Backend(format!("BEGIN IMMEDIATE: {err}")))?;

        let result: Result<RawLeaseRow, LeaseError> = async {
            let existing = Self::read_raw_row(&conn, tenant)
                .await?
                .ok_or_else(|| LeaseError::NotFound(tenant.clone()))?;

            // Enforce DedicatedCell / SharedPool role policy on the target holder.
            let role = CellRole::parse(&existing.cell_role)
                .map_err(|e| LeaseError::Backend(format!("stored cell_role invalid: {e}")))?;
            let mode = match existing.placement_mode.as_str() {
                "shared_pool" => PlacementMode::SharedPool,
                "dedicated_cell" => PlacementMode::DedicatedCell,
                other => {
                    return Err(LeaseError::Backend(format!(
                        "unknown placement_mode: {other}"
                    )));
                }
            };
            if !spiffe_matches_cell_role(&to_holder.spiffe_id, &role, mode) {
                return Err(LeaseError::HolderMismatch(tenant.clone()));
            }

            let now_ms = Self::db_now_ms(&conn).await?;
            let new_expires = now_ms.saturating_add(ttl_i64);

            let affected = conn
                .execute(
                    "UPDATE placement_leases
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
                    (
                        to_holder.spiffe_id.as_str(),
                        to_holder.incarnation_pubkey.as_bytes().to_vec(),
                        to_holder.tls_exporter_proof.as_bytes().to_vec(),
                        new_expires,
                        now_ms,
                        tenant.as_str(),
                        from_holder.spiffe_id.as_str(),
                        from_holder.incarnation_pubkey.as_bytes().to_vec(),
                        gen_i64,
                    ),
                )
                .await
                .map_err(|err| LeaseError::Backend(format!("transfer update failed: {err}")))?;

            if affected == 1 {
                Self::read_raw_row(&conn, tenant)
                    .await?
                    .ok_or_else(|| LeaseError::Backend("transferred row disappeared".to_owned()))
            } else {
                Err(
                    Self::classify_cas_failure(&conn, tenant, from_holder, expected_generation)
                        .await,
                )
            }
        }
        .await;

        match result {
            Ok(raw) => {
                if let Err(err) = conn.execute("COMMIT", ()).await {
                    let _ = conn.execute("ROLLBACK", ()).await;
                    return Err(LeaseError::Backend(format!(
                        "transfer COMMIT failed: {err}"
                    )));
                }
                raw.into_lease(tenant)
            }
            Err(err) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(err)
            }
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
        let conn = self.conn.lock().await;
        conn.execute("BEGIN IMMEDIATE", ())
            .await
            .map_err(|err| LeaseError::Backend(format!("BEGIN IMMEDIATE: {err}")))?;

        let result: Result<(), LeaseError> = async {
            let now_ms = Self::db_now_ms(&conn).await?;

            // Never delete the row: preserve monotonic `generation` across releases.
            let affected = conn
                .execute(
                    "UPDATE placement_leases
                     SET transfer_state = 'released',
                         expires_at_ms = 0,
                         transfer_target_json = NULL,
                         updated_at_ms = ?1
                     WHERE tenant_id = ?2
                       AND holder_spiffe_id = ?3
                       AND holder_incarnation_pubkey = ?4
                       AND generation = ?5
                       AND transfer_state != 'released'",
                    (
                        now_ms,
                        tenant.as_str(),
                        holder.spiffe_id.as_str(),
                        holder.incarnation_pubkey.as_bytes().to_vec(),
                        gen_i64,
                    ),
                )
                .await
                .map_err(|err| LeaseError::Backend(format!("release update failed: {err}")))?;

            if affected == 1 {
                Ok(())
            } else {
                Err(Self::classify_cas_failure(&conn, tenant, holder, expected_generation).await)
            }
        }
        .await;

        match result {
            Ok(()) => {
                if let Err(err) = conn.execute("COMMIT", ()).await {
                    let _ = conn.execute("ROLLBACK", ()).await;
                    return Err(LeaseError::Backend(format!("release COMMIT failed: {err}")));
                }
                Ok(())
            }
            Err(err) => {
                let _ = conn.execute("ROLLBACK", ()).await;
                Err(err)
            }
        }
    }

    async fn current(&self, tenant: &TenantId) -> Result<Option<PlacementLease>, LeaseError> {
        let conn = self.conn.lock().await;
        let now_ms = Self::db_now_ms(&conn).await?;
        let Some(raw) = Self::read_raw_row(&conn, tenant).await? else {
            return Ok(None);
        };
        if raw.transfer_state == "released" || raw.expires_at_ms <= now_ms {
            return Ok(None);
        }
        raw.into_lease(tenant).map(Some)
    }
}

/// Storage-host generation check helper verifying that `scope` matches the
/// authoritative `tenant_placement_fence` singleton row inside a tenant's
/// Turso WAL database.
pub async fn verify_turso_write_fence(
    conn: &turso::Connection,
    scope: &TenantQueryScope,
) -> Result<(), TenantStoreError> {
    let gen_i64 = i64::try_from(scope.placement_generation.get())
        .map_err(|_| TenantStoreError::Backend("placement generation overflow".to_owned()))?;
    let mut rows = conn
        .query(
            "SELECT 1 FROM tenant_placement_fence
             WHERE singleton = 1
               AND holder_spiffe_id = ?1
               AND holder_incarnation_pubkey = ?2
               AND generation = ?3",
            (
                scope.holder.spiffe_id.as_str(),
                scope.holder.incarnation_pubkey.as_bytes().to_vec(),
                gen_i64,
            ),
        )
        .await
        .map_err(|err| TenantStoreError::Backend(format!("turso write fence check: {err}")))?;
    if rows
        .next()
        .await
        .map_err(|err| TenantStoreError::Backend(format!("turso write fence row: {err}")))?
        .is_some()
    {
        Ok(())
    } else {
        Err(TenantStoreError::StalePlacementGeneration {
            tenant_id: scope.tenant_id.clone(),
            expected: scope.placement_generation,
            actual: FenceGeneration::ZERO,
        })
    }
}
