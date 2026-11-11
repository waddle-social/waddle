//! Keypair-slot Postgres CAS lease (ADR-0017 element 3/4).
//!
//! A pod leases exactly one slot of the pre-enrolled keypair pool via a
//! server-time (`now()`) compare-and-set, so at most one live node ever holds
//! a given slot — and therefore no two swarm members share a libp2p `PeerId`.
//! The lease is heartbeat-renewed; a renewal that finds its slot stolen or
//! expired is **fencing loss** and the node must stop using that identity.
//!
//! Postgres-only: the control plane has no SQLite equivalent (`now()`-based
//! row CAS), and clustering requires Postgres. The store goes through the
//! portable `Database` (which rewrites `?`→`$n` for Postgres and leaves
//! `now()`/interval SQL intact); it is only ever invoked on a Postgres
//! deployment.
//!
//! The dedicated control-plane connection pool the ADR prescribes (element
//! 4/12) lands here (ADR-0017 Phase 3 Slice 0): [`PostgresKeypairSlotLease::heartbeat`]
//! renews through [`Database::control_plane_guard`], so this per-node
//! liveness beat never queues behind fenced bulk writes on the main pool.
//! `acquire`/`release` are not the periodic liveness beat the pool split
//! protects and stay on the main pool.

use super::NodeId;
use crate::db::physical_attestation::AttestedPostgresWriterCapability;
use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};
use async_trait::async_trait;
use libp2p::PeerId;
use std::collections::HashMap;
use std::str::FromStr;
use std::time::Duration;

#[cfg(all(test, feature = "test-support"))]
#[path = "lease_runtime_attach_tests.rs"]
mod runtime_attach_tests;

/// Per-process leaseholder identity. `node_id`/`node_epoch` are freshly
/// generated on every process start and never reused, so a restarted pod never
/// looks like its former self holding a slot. Both are typed values
/// (typed-payloads rule); serialization to TEXT happens only at the SQL
/// parameter boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeaseIdentity {
    pub node_id: NodeId,
    pub node_epoch: uuid::Uuid,
}

/// A successfully leased keypair-pool slot (index into the enrolled pool).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LeasedSlot {
    pub slot_index: u32,
}

#[derive(Debug, Clone)]
struct EnrolledKeypairPool {
    generation: uuid::Uuid,
    peer_ids: Vec<PeerId>,
}

/// Keypair-slot lease failures.
#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    /// The backing database returned an error.
    #[error("clustering keypair-slot lease database error: {0}")]
    Database(#[from] DatabaseError),
    /// Every pool slot is currently held by a live node.
    #[error(
        "no free clustering keypair-slot could be leased: all {pool_size} enrolled pool slots \
         are held by live nodes (scale-up beyond the enrolled pool needs a pipeline enrollment run)"
    )]
    NoFreeSlot { pool_size: usize },
    /// A heartbeat renewal found the slot no longer held by this node
    /// (stolen after lease expiry, or epoch superseded).
    #[error("clustering keypair-slot {slot_index} lease lost (fencing): slot no longer held by this node")]
    FencingLoss { slot_index: u32 },
    /// `acquire` was called with an empty pool — a configuration error the
    /// caller should have screened out.
    #[error("clustering keypair-slot lease requested with an empty keypair pool")]
    EmptyPool,
    /// Owner DDL must not run through an attested restricted runtime handle.
    #[error(
        "keypair-slot owner schema initialization requires a non-attested owner database handle"
    )]
    OwnerBootstrapRequiresOwnerHandle,
    /// An attested runtime handle can operate only after exact main and
    /// control-plane pool verification by `attach_existing`.
    #[error(
        "attested keypair-slot runtime handle must pass attach_existing before lease operations"
    )]
    RuntimeAttachRequired,
    /// The keypair pool contains duplicate or no PeerIds.
    #[error("keypair-pool enrollment requires a non-empty unique PeerId order")]
    InvalidKeypairPool,
}

/// Fail-closed refusal to attach a restricted keypair-slot runtime store.
#[derive(Debug, thiserror::Error)]
pub enum LeaseAttachError {
    #[error("keypair-slot lease attachment requires PostgreSQL")]
    PostgresRequired,
    #[error("keypair-slot lease attachment requires the attested main database lane")]
    MainLaneRequired,
    #[error("keypair-slot lease attachment requires an attested main pool")]
    AttestedMainPoolRequired,
    #[error("keypair-slot lease attachment requires an independent attested control-plane pool")]
    AttestedControlPlanePoolRequired,
    #[error("keypair-slot lease main and control-plane pool identities differ")]
    PhysicalIdentityMismatch,
    #[error("the attested schema identifier is invalid")]
    InvalidSchema,
    #[error("the active PostgreSQL schema differs from the attested schema")]
    ActiveSchemaMismatch,
    #[error("could not verify the PostgreSQL {setting} bound for keypair-slot lease attachment")]
    InvalidTimeoutSetting { setting: &'static str },
    #[error("the clustering keypair-slot relation is missing or has an invalid shape")]
    InvalidSchemaShape,
    #[error("the clustering keypair-slot runtime role or grants are unsafe")]
    UnsafeRuntimeRole,
    #[error("the owner has not activated this keypair-pool generation")]
    KeypoolGenerationPending,
    #[error("the configured PeerId order does not match owner enrollment")]
    KeypairPoolOrderMismatch,
    #[error("the configured keypair pool is empty, malformed, or contains duplicate PeerIds")]
    InvalidConfiguredKeypairPool,
    #[error("the keypair-pool enrollment relations have an invalid shape")]
    InvalidKeypairPoolShape,
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// Fail-closed owner enrollment failures for a keypair-pool generation.
#[derive(Debug, thiserror::Error)]
pub enum KeypoolOwnerError {
    #[error("keypair-pool owner enrollment requires PostgreSQL")]
    PostgresRequired,
    #[error("keypair-pool owner enrollment requires a non-attested owner handle")]
    OwnerHandleRequired,
    #[error("the keypair-pool owner identity or schema is unsafe")]
    UnsafeOwnerBoundary,
    #[error("keypair-pool enrollment requires a non-empty unique PeerId order")]
    InvalidPeerOrder,
    #[error("a legacy keypair-slot or keypool authority already exists")]
    ExistingAuthority,
    #[error("keypair-pool generation is not ready for activation")]
    ActivationNotReady,
    #[error("keypair-pool generation is missing or already active")]
    InvalidGenerationState,
    #[error(
        "a keypair-slot table created by an unattested runtime exists and the plan does not \
         retire it"
    )]
    LegacyKeypairSlots,
    #[error("the unattested keypair-slot table does not have the shape this owner can retire")]
    LegacyShapeMismatch,
    #[error(
        "keypair slot {slot} is still leased by {holder}; its last heartbeat was {age_ms} ms ago \
         on the database clock"
    )]
    LegacyLeaseLive {
        slot: i32,
        holder: String,
        age_ms: i64,
    },
    #[error("the activation delay is shorter than the lease lifetime being retired")]
    ActivationDelayBelowLegacyTtl,
    #[error("the enrolled keypair pool has a different generation or PeerId order")]
    EnrollmentMismatch,
    #[error("the enrolled keypair-pool relations or guards do not have the expected shape")]
    EnrolledShape(#[source] LeaseAttachError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// What the owner does with a `clustering_keypair_slots` table that has no
/// enrollment: the table an unattested runtime creates for itself, whose rows
/// name no enrolled generation or PeerId.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LegacyKeypairSlots {
    /// Refuse the preparation and leave the table untouched.
    #[default]
    Refuse,
    /// Drop the table and create the enrolled one, but only while no row is
    /// leased. A row is leased when it has a holder, is not marked expired
    /// and its heartbeat is newer than `legacy_ttl` on the database clock;
    /// `legacy_ttl` must be at least the lease lifetime the unattested
    /// runtime was configured with. Holder history in the table is lost.
    Retire { legacy_ttl: Duration },
}

/// The keypair pool an owner enrolls or adopts.
#[derive(Debug, Clone, Copy)]
pub struct KeypoolEnrollment<'a> {
    /// Generation the enrollment is recorded under.
    pub generation: uuid::Uuid,
    /// Ordered PeerIds; slot `n` leases `peer_ids[n]`.
    pub peer_ids: &'a [PeerId],
    /// Restricted login that leases slots.
    pub runtime_role: &'a str,
    /// How long a newly enrolled generation stays pending.
    pub activation_delay: Duration,
    pub legacy: LegacyKeypairSlots,
}

/// State of the enrolled generation after an owner preparation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeypoolPreparation {
    /// Runtime attachment accepts the generation.
    Active,
    /// The activation delay has not elapsed; a later preparation with the
    /// same enrollment activates it.
    Pending,
}

/// The keypair-slot lease store. A trait so a dedicated control-plane pool
/// implementation (Phase 3) or a test double can substitute without touching
/// callers.
#[async_trait]
pub trait KeypairSlotLease: Send + Sync {
    /// Create the backing table if it does not exist.
    async fn ensure_schema(&self) -> Result<(), LeaseError>;

    /// Lease any free or expired slot in `[0, pool_size)`. Returns the leased
    /// slot, or [`LeaseError::NoFreeSlot`] if all are held by live nodes.
    async fn acquire(
        &self,
        identity: &LeaseIdentity,
        pool_size: usize,
        lease_ttl: Duration,
    ) -> Result<LeasedSlot, LeaseError>;

    /// Renew the lease on a held slot. Returns [`LeaseError::FencingLoss`] when
    /// the freshness-gated CAS affects zero rows (lease lapsed or stolen).
    async fn heartbeat(
        &self,
        identity: &LeaseIdentity,
        slot: LeasedSlot,
        lease_ttl: Duration,
    ) -> Result<(), LeaseError>;

    /// Release a held slot on graceful drain (epoch-gated, best-effort).
    async fn release(&self, identity: &LeaseIdentity, slot: LeasedSlot) -> Result<(), LeaseError>;
}

/// Postgres implementation using `now()`-based row CAS.
pub struct PostgresKeypairSlotLease {
    db: Database,
    runtime_attach_verified: bool,
    enrolled_pool: Option<EnrolledKeypairPool>,
}

impl PostgresKeypairSlotLease {
    pub fn new(db: Database) -> Self {
        Self {
            db,
            runtime_attach_verified: false,
            enrolled_pool: None,
        }
    }

    /// Initialize the keypair-slot table through this exact owner database.
    /// Attested runtime handles can attach only after the owner has prepared
    /// the schema and grants.
    pub async fn initialize_owner(db: Database) -> Result<Self, LeaseError> {
        if db.attested_pg_writer_capability().is_some() {
            return Err(LeaseError::OwnerBootstrapRequiresOwnerHandle);
        }
        let store = Self {
            db,
            runtime_attach_verified: false,
            enrolled_pool: None,
        };
        store.ensure_schema().await?;
        Ok(store)
    }

    /// Install a fresh owner-controlled keypair mapping and its pending
    /// generation. This path refuses every pre-existing lease or keypool
    /// relation; it does not reinterpret a live legacy lease table.
    ///
    /// `activation_delay` is the operator supplied drain-and-fence window for
    /// any previous binaries. The generation remains pending until
    /// [`Self::activate_keypool_owner`] is called after that window and the
    /// external full-stop procedure has completed.
    pub async fn initialize_keypool_owner(
        db: &Database,
        identity: &crate::db::lineage::PgIdentity,
        generation: uuid::Uuid,
        peer_ids: &[PeerId],
        runtime_role: &str,
        activation_delay: Duration,
    ) -> Result<(), KeypoolOwnerError> {
        if db.driver() != DatabaseDriver::Postgres {
            return Err(KeypoolOwnerError::PostgresRequired);
        }
        if db.attested_pg_writer_capability().is_some() {
            return Err(KeypoolOwnerError::OwnerHandleRequired);
        }
        validate_peer_order(peer_ids).map_err(|_| KeypoolOwnerError::InvalidPeerOrder)?;
        crate::db::physical_attestation::validate_identifier(runtime_role, "keypool runtime role")
            .map_err(|_| KeypoolOwnerError::UnsafeOwnerBoundary)?;
        let delay_ms = i64::try_from(activation_delay.as_millis())
            .map_err(|_| KeypoolOwnerError::InvalidPeerOrder)?;
        initialize_keypool_owner(db, identity, generation, peer_ids, runtime_role, delay_ms).await
    }

    /// Complete the owner-only pending-to-active transition after the
    /// externally verified full-stop and drain window. Runtime attachment and
    /// lease mutation both refuse a pending generation.
    pub async fn activate_keypool_owner(
        db: &Database,
        identity: &crate::db::lineage::PgIdentity,
        generation: uuid::Uuid,
    ) -> Result<(), KeypoolOwnerError> {
        if db.driver() != DatabaseDriver::Postgres {
            return Err(KeypoolOwnerError::PostgresRequired);
        }
        if db.attested_pg_writer_capability().is_some() {
            return Err(KeypoolOwnerError::OwnerHandleRequired);
        }
        activate_keypool_owner(db, identity, generation).await
    }

    /// Bring the keypair authority to the active enrollment `enrollment`
    /// names, creating, adopting or completing it as needed:
    ///
    /// - nothing exists: the tables, guards and grants are created and the
    ///   generation is enrolled pending;
    /// - only an unenrolled lease table exists: refused, or retired under
    ///   [`LegacyKeypairSlots::Retire`] and then enrolled pending;
    /// - an enrollment exists: its shape and guards are verified, guard
    ///   functions that run with the owner's privileges are switched to
    ///   run with the caller's, and a different generation or PeerId order is
    ///   refused.
    ///
    /// A pending generation is activated once its delay has elapsed and the
    /// lease table is empty. Repeating the call on an active matching
    /// enrollment changes nothing, and lease rows are never rewritten.
    pub async fn prepare_keypool_owner(
        db: &Database,
        identity: &crate::db::lineage::PgIdentity,
        enrollment: &KeypoolEnrollment<'_>,
    ) -> Result<KeypoolPreparation, KeypoolOwnerError> {
        if db.driver() != DatabaseDriver::Postgres {
            return Err(KeypoolOwnerError::PostgresRequired);
        }
        if db.attested_pg_writer_capability().is_some() {
            return Err(KeypoolOwnerError::OwnerHandleRequired);
        }
        validate_peer_order(enrollment.peer_ids)
            .map_err(|_| KeypoolOwnerError::InvalidPeerOrder)?;
        crate::db::physical_attestation::validate_identifier(
            enrollment.runtime_role,
            "keypool runtime role",
        )
        .map_err(|_| KeypoolOwnerError::UnsafeOwnerBoundary)?;
        prepare_keypool_owner(db, identity, enrollment).await
    }

    /// Switch keypair guard functions that carry `SECURITY DEFINER` to run
    /// with the calling role's privileges, in one owner transaction, and
    /// report how many were switched. Nothing else about the enrollment is
    /// read or changed; [`Self::prepare_keypool_owner`] verifies the exact
    /// guard pin afterwards. An owner phase runs this before any check that
    /// refuses owner-privileged triggers on relations the runtime writes.
    pub async fn convert_definer_guards_owner(
        db: &Database,
        identity: &crate::db::lineage::PgIdentity,
        runtime_role: &str,
    ) -> Result<usize, KeypoolOwnerError> {
        if db.driver() != DatabaseDriver::Postgres {
            return Err(KeypoolOwnerError::PostgresRequired);
        }
        if db.attested_pg_writer_capability().is_some() {
            return Err(KeypoolOwnerError::OwnerHandleRequired);
        }
        crate::db::physical_attestation::validate_identifier(runtime_role, "keypool runtime role")
            .map_err(|_| KeypoolOwnerError::UnsafeOwnerBoundary)?;
        let mut tx = begin_keypool_owner(db, identity, runtime_role).await?;
        let converted = convert_definer_guards(&mut tx, &identity.schema.name.0).await?;
        tx.commit().await?;
        Ok(converted)
    }

    /// Attach the exact attested main and independent control-plane pools to
    /// an owner-prepared table. `configured_keypair_pool` is the ordered
    /// base64 secret list from clustering configuration; this method derives
    /// its PeerIds before any lease can be acquired. Attachment performs only
    /// bounded read-only catalog checks and no URL reopen or schema DDL.
    pub async fn attach_existing(
        db: Database,
        configured_keypair_pool: &[String],
    ) -> Result<Self, LeaseAttachError> {
        if db.driver() != crate::db::DatabaseDriver::Postgres {
            return Err(LeaseAttachError::PostgresRequired);
        }
        let expected_peer_ids = configured_peer_order(configured_keypair_pool)?;
        if !db.is_main_lane() {
            return Err(LeaseAttachError::MainLaneRequired);
        }
        let main_capability = db
            .attested_pg_writer_capability()
            .ok_or(LeaseAttachError::AttestedMainPoolRequired)?;
        db.verify_attested_pg_writer_capability(&main_capability)
            .map_err(DatabaseError::from)?;
        let (control_plane, control_plane_capability) = db
            .attested_control_plane_pool_view()
            .map_err(|_| LeaseAttachError::AttestedControlPlanePoolRequired)?;
        if main_capability.identity() != control_plane_capability.identity() {
            return Err(LeaseAttachError::PhysicalIdentityMismatch);
        }

        let enrolled_pool = verify_attested_keypair_slot_pair(
            &db,
            &main_capability,
            &control_plane,
            &control_plane_capability,
            &expected_peer_ids,
        )
        .await?;
        Ok(Self {
            db,
            runtime_attach_verified: true,
            enrolled_pool: Some(enrolled_pool),
        })
    }

    fn require_runtime_attach(&self) -> Result<(), LeaseError> {
        if self.db.attested_pg_writer_capability().is_some() && !self.runtime_attach_verified {
            return Err(LeaseError::RuntimeAttachRequired);
        }
        Ok(())
    }

    /// Try to claim one specific slot. Returns `true` on success (row inserted
    /// or a stale/free row stolen), `false` if the slot is held fresh.
    async fn try_acquire_slot(
        &self,
        identity: &LeaseIdentity,
        slot_index: u32,
        lease_ttl: Duration,
    ) -> Result<bool, LeaseError> {
        let conn = self.db.guard().await?;
        if let Some(pool) = &self.enrolled_pool {
            let peer_id = pool
                .peer_ids
                .get(slot_index as usize)
                .ok_or(LeaseError::InvalidKeypairPool)?;
            let affected = conn
                .execute(
                    r#"
                    INSERT INTO clustering_keypair_slots
                        (slot_index, holder_node, holder_epoch, heartbeat, expired,
                         keypool_generation, peer_id, lease_revision)
                    VALUES (?, ?, ?, now(), false, ?::uuid, ?, 0)
                    ON CONFLICT (slot_index) DO UPDATE SET
                        holder_node = EXCLUDED.holder_node,
                        holder_epoch = EXCLUDED.holder_epoch,
                        heartbeat = now(),
                        expired = false,
                        keypool_generation = EXCLUDED.keypool_generation,
                        peer_id = EXCLUDED.peer_id,
                        lease_revision = clustering_keypair_slots.lease_revision + 1
                    WHERE clustering_keypair_slots.holder_node IS NULL
                       OR clustering_keypair_slots.expired
                       OR clustering_keypair_slots.heartbeat < now() - (? || ' milliseconds')::interval
                    "#,
                    crate::db_params![
                        i64::from(slot_index),
                        identity.node_id.as_str().to_string(),
                        identity.node_epoch.to_string(),
                        pool.generation.to_string(),
                        peer_id.to_string(),
                        lease_ttl.as_millis().to_string(),
                    ],
                )
                .await?;
            return Ok(affected == 1);
        }
        // Upsert CAS: insert a fresh hold, or steal an existing row only when
        // it is unheld, already expired, or its heartbeat has aged past the
        // TTL. A held-and-fresh slot fails the ON CONFLICT WHERE and affects
        // zero rows. Postgres row-locks the conflicting row, so two racing
        // acquirers of the same slot serialize and exactly one wins.
        let affected = conn
            .execute(
                r#"
                INSERT INTO clustering_keypair_slots
                    (slot_index, holder_node, holder_epoch, heartbeat, expired)
                VALUES (?, ?, ?, now(), false)
                ON CONFLICT (slot_index) DO UPDATE SET
                    holder_node = EXCLUDED.holder_node,
                    holder_epoch = EXCLUDED.holder_epoch,
                    heartbeat = now(),
                    expired = false
                WHERE clustering_keypair_slots.holder_node IS NULL
                   OR clustering_keypair_slots.expired
                   OR clustering_keypair_slots.heartbeat < now() - (? || ' milliseconds')::interval
                "#,
                // Storage boundary: typed identity serializes to TEXT here.
                crate::db_params![
                    i64::from(slot_index),
                    identity.node_id.as_str().to_string(),
                    identity.node_epoch.to_string(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await?;
        Ok(affected == 1)
    }
}

#[async_trait]
impl KeypairSlotLease for PostgresKeypairSlotLease {
    async fn ensure_schema(&self) -> Result<(), LeaseError> {
        if self.db.attested_pg_writer_capability().is_some() {
            return Err(LeaseError::OwnerBootstrapRequiresOwnerHandle);
        }
        let conn = self.db.guard().await?;
        conn.execute(
            r#"
            CREATE TABLE IF NOT EXISTS clustering_keypair_slots (
                slot_index   INTEGER PRIMARY KEY,
                holder_node  TEXT,
                holder_epoch TEXT,
                heartbeat    TIMESTAMPTZ NOT NULL DEFAULT now(),
                expired      BOOLEAN NOT NULL DEFAULT TRUE
            )
            "#,
            (),
        )
        .await?;
        Ok(())
    }

    async fn acquire(
        &self,
        identity: &LeaseIdentity,
        pool_size: usize,
        lease_ttl: Duration,
    ) -> Result<LeasedSlot, LeaseError> {
        self.require_runtime_attach()?;
        if pool_size == 0 {
            return Err(LeaseError::EmptyPool);
        }
        if self
            .enrolled_pool
            .as_ref()
            .is_some_and(|enrollment| enrollment.peer_ids.len() != pool_size)
        {
            return Err(LeaseError::InvalidKeypairPool);
        }
        // Scan slots, starting at an offset derived from this node's id so
        // concurrently-starting pods don't all contend on slot 0 first. The
        // CAS guarantees correctness regardless of scan order.
        let start = (stable_offset(identity.node_id.as_str()) % pool_size) as u32;
        let pool = pool_size as u32;
        for step in 0..pool {
            let slot_index = (start + step) % pool;
            if self
                .try_acquire_slot(identity, slot_index, lease_ttl)
                .await?
            {
                return Ok(LeasedSlot { slot_index });
            }
        }
        Err(LeaseError::NoFreeSlot { pool_size })
    }

    async fn heartbeat(
        &self,
        identity: &LeaseIdentity,
        slot: LeasedSlot,
        lease_ttl: Duration,
    ) -> Result<(), LeaseError> {
        self.require_runtime_attach()?;
        // Runs on the dedicated control-plane pool (ADR-0017 element 4/12,
        // Phase 3 Slice 0): this per-node liveness renewal must never queue
        // behind fenced bulk writes, backstop fencing SELECTs, or claims-read
        // storms on the main pool.
        let conn = self.db.control_plane_guard().await?;
        // Freshness-gated renewal CAS (ADR element 4): renew only while we
        // still hold the slot under our epoch and the lease has not lapsed.
        let affected = if let Some(pool) = &self.enrolled_pool {
            let peer_id = pool
                .peer_ids
                .get(slot.slot_index as usize)
                .ok_or(LeaseError::InvalidKeypairPool)?;
            conn.execute(
                r#"
                UPDATE clustering_keypair_slots
                SET heartbeat = now(), lease_revision = lease_revision + 1
                WHERE slot_index = ?
                  AND holder_node = ?
                  AND holder_epoch = ?
                  AND keypool_generation = ?::uuid
                  AND peer_id = ?
                  AND NOT expired
                  AND heartbeat >= now() - (? || ' milliseconds')::interval
                "#,
                crate::db_params![
                    i64::from(slot.slot_index),
                    identity.node_id.as_str().to_string(),
                    identity.node_epoch.to_string(),
                    pool.generation.to_string(),
                    peer_id.to_string(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await?
        } else {
            conn.execute(
                r#"
                UPDATE clustering_keypair_slots
                SET heartbeat = now()
                WHERE slot_index = ?
                  AND holder_node = ?
                  AND holder_epoch = ?
                  AND NOT expired
                  AND heartbeat >= now() - (? || ' milliseconds')::interval
                "#,
                crate::db_params![
                    i64::from(slot.slot_index),
                    identity.node_id.as_str().to_string(),
                    identity.node_epoch.to_string(),
                    lease_ttl.as_millis().to_string(),
                ],
            )
            .await?
        };
        if affected == 0 {
            return Err(LeaseError::FencingLoss {
                slot_index: slot.slot_index,
            });
        }
        Ok(())
    }

    async fn release(&self, identity: &LeaseIdentity, slot: LeasedSlot) -> Result<(), LeaseError> {
        self.require_runtime_attach()?;
        let conn = self.db.guard().await?;
        // Epoch-gated release: mark the slot free+expired so a replacement can
        // acquire it immediately, but only if we still own it.
        if let Some(pool) = &self.enrolled_pool {
            let peer_id = pool
                .peer_ids
                .get(slot.slot_index as usize)
                .ok_or(LeaseError::InvalidKeypairPool)?;
            conn.execute(
                r#"
                UPDATE clustering_keypair_slots
                SET holder_node = NULL, holder_epoch = NULL, expired = true,
                    lease_revision = lease_revision + 1
                WHERE slot_index = ? AND holder_node = ? AND holder_epoch = ?
                  AND keypool_generation = ?::uuid AND peer_id = ?
                "#,
                crate::db_params![
                    i64::from(slot.slot_index),
                    identity.node_id.as_str().to_string(),
                    identity.node_epoch.to_string(),
                    pool.generation.to_string(),
                    peer_id.to_string(),
                ],
            )
            .await?;
        } else {
            conn.execute(
                r#"
                UPDATE clustering_keypair_slots
                SET holder_node = NULL, holder_epoch = NULL, expired = true
                WHERE slot_index = ? AND holder_node = ? AND holder_epoch = ?
                "#,
                crate::db_params![
                    i64::from(slot.slot_index),
                    identity.node_id.as_str().to_string(),
                    identity.node_epoch.to_string(),
                ],
            )
            .await?;
        }
        Ok(())
    }
}

const KEYPAIR_SLOT_TABLE: &str = "clustering_keypair_slots";
const KEYPAIR_POOL_TABLE: &str = "clustering_keypool";
const KEYPAIR_POOL_SLOTS_TABLE: &str = "clustering_keypool_slots";
const KEYPAIR_GUARD_FUNCTION: &str = "clustering_keypair_slot_guard";
const KEYPAIR_GUARD_TRIGGER: &str = "clustering_keypair_slot_guard_trigger";
const KEYPAIR_POOL_GUARD_FUNCTION: &str = "clustering_keypool_guard";
const KEYPAIR_POOL_GUARD_TRIGGER: &str = "clustering_keypool_guard_trigger";
const KEYPAIR_MAP_GUARD_FUNCTION: &str = "clustering_keypool_slots_guard";
const KEYPAIR_MAP_GUARD_TRIGGER: &str = "clustering_keypool_slots_guard_trigger";
const KEYPAIR_POOL_TRUNCATE_TRIGGER: &str = "clustering_keypool_truncate_guard_trigger";
const KEYPAIR_MAP_TRUNCATE_TRIGGER: &str = "clustering_keypool_slots_truncate_guard_trigger";
const KEYPAIR_SLOT_TRUNCATE_TRIGGER: &str = "clustering_keypair_slot_truncate_guard_trigger";
const KEYPAIR_TRUNCATE_GUARD_FUNCTION: &str = "clustering_keypair_truncate_guard";
const KEYPAIR_OWNER_ADVISORY_LOCK: i64 = 6_841_445_497_037_938_001;

const KEYPAIR_GUARD_BODY: &str = r#"
DECLARE
    expected_generation uuid;
    expected_peer_id text;
BEGIN
    IF TG_OP = 'DELETE' THEN
        RAISE EXCEPTION 'keypair slot rows cannot be deleted'
            USING ERRCODE = '55000';
    END IF;
    SELECT keypool.generation, keyslots.peer_id
      INTO expected_generation, expected_peer_id
      FROM clustering_keypool keypool
      JOIN clustering_keypool_slots keyslots
        ON keyslots.generation = keypool.generation
     WHERE keypool.singleton
       AND keypool.state = 'active'
       AND keyslots.slot_index = NEW.slot_index;
    IF NOT FOUND
       OR NEW.keypool_generation IS DISTINCT FROM expected_generation
       OR NEW.peer_id IS DISTINCT FROM expected_peer_id THEN
        RAISE EXCEPTION 'keypair slot does not match the active enrolled generation'
            USING ERRCODE = '55000';
    END IF;
    IF TG_OP = 'INSERT' THEN
        IF NEW.lease_revision <> 0 THEN
            RAISE EXCEPTION 'new keypair slot revision must start at zero'
                USING ERRCODE = '55000';
        END IF;
    ELSIF NEW.lease_revision <> OLD.lease_revision + 1 THEN
        RAISE EXCEPTION 'keypair slot update requires a fresh lease revision'
            USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
"#;

const KEYPAIR_TRUNCATE_GUARD_BODY: &str = r#"
BEGIN
    RAISE EXCEPTION 'keypair enrollment relations cannot be truncated'
        USING ERRCODE = '55000';
    RETURN NULL;
END;
"#;

const KEYPAIR_POOL_GUARD_BODY: &str = r#"
BEGIN
    IF TG_OP = 'DELETE'
       OR OLD.state <> 'pending'
       OR NEW.state <> 'active'
       OR NEW.singleton IS DISTINCT FROM OLD.singleton
       OR NEW.generation IS DISTINCT FROM OLD.generation
       OR NEW.slot_count IS DISTINCT FROM OLD.slot_count
       OR NEW.activation_after IS DISTINCT FROM OLD.activation_after
       OR NEW.created_at IS DISTINCT FROM OLD.created_at THEN
        RAISE EXCEPTION 'keypair pool generation is immutable after enrollment'
            USING ERRCODE = '55000';
    END IF;
    RETURN NEW;
END;
"#;

const KEYPAIR_MAP_GUARD_BODY: &str = r#"
DECLARE
    generation_state text;
BEGIN
    IF TG_OP = 'INSERT' THEN
        SELECT state INTO generation_state
          FROM clustering_keypool
         WHERE singleton AND generation = NEW.generation;
    ELSE
        SELECT state INTO generation_state
          FROM clustering_keypool
         WHERE singleton AND generation = OLD.generation;
    END IF;
    IF generation_state IS DISTINCT FROM 'pending' THEN
        RAISE EXCEPTION 'keypair pool mapping is immutable outside pending enrollment'
            USING ERRCODE = '55000';
    END IF;
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    END IF;
    RETURN NEW;
END;
"#;

/// Which keypair authority objects the schema already holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum KeypairInventory {
    /// No lease table, enrollment table, guard function or guard trigger.
    Absent,
    /// Only the lease table, with none of the enrollment objects: the table
    /// an unattested runtime creates for itself.
    LegacySlotsOnly,
    /// At least one enrollment table, guard function or guard trigger.
    Enrolled,
}

/// Open the owner transaction every keypool preparation runs in: the
/// keypool advisory lock, the pinned owner schema, schema USAGE for the
/// runtime and the runtime role checks.
async fn begin_keypool_owner<'a>(
    db: &'a Database,
    identity: &crate::db::lineage::PgIdentity,
    runtime_role: &str,
) -> Result<Transaction<'a>, KeypoolOwnerError> {
    let mut tx = db.begin().await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE",
        (),
    )
    .await?;
    tx.query(
        "SELECT pg_catalog.pg_advisory_xact_lock(?)",
        crate::db_params![KEYPAIR_OWNER_ADVISORY_LOCK],
    )
    .await?;
    crate::room_effect_outbox::pin_room_owner_schema(&mut tx, identity)
        .await
        .map_err(|_| KeypoolOwnerError::UnsafeOwnerBoundary)?;
    let schema_name = &identity.schema.name.0;
    let schema = crate::db::physical_attestation::quote_identifier(schema_name);
    tx.execute(
        &format!("GRANT USAGE ON SCHEMA {schema} TO {runtime_role}"),
        (),
    )
    .await?;
    verify_keypool_runtime_candidate(&mut tx, schema_name, runtime_role).await?;
    Ok(tx)
}

async fn keypair_inventory(
    tx: &mut Transaction<'_>,
    schema_name: &str,
) -> Result<KeypairInventory, KeypoolOwnerError> {
    let mut existing = tx
        .query(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
               JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
               WHERE n.nspname = ? AND c.relname = ?), \
             EXISTS (SELECT 1 FROM pg_catalog.pg_class c \
               JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
               WHERE n.nspname = ? AND c.relname IN (?, ?)) \
             OR EXISTS (SELECT 1 FROM pg_catalog.pg_proc p \
               JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
               WHERE n.nspname = ? AND p.proname IN (?, ?, ?, ?)) \
             OR EXISTS (SELECT 1 FROM pg_catalog.pg_trigger t \
               JOIN pg_catalog.pg_class c ON c.oid = t.tgrelid \
               JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
               WHERE n.nspname = ? AND t.tgname IN (?, ?, ?, ?, ?, ?))",
            crate::db_params![
                schema_name,
                KEYPAIR_SLOT_TABLE,
                schema_name,
                KEYPAIR_POOL_TABLE,
                KEYPAIR_POOL_SLOTS_TABLE,
                schema_name,
                KEYPAIR_GUARD_FUNCTION,
                KEYPAIR_POOL_GUARD_FUNCTION,
                KEYPAIR_MAP_GUARD_FUNCTION,
                KEYPAIR_TRUNCATE_GUARD_FUNCTION,
                schema_name,
                KEYPAIR_GUARD_TRIGGER,
                KEYPAIR_POOL_GUARD_TRIGGER,
                KEYPAIR_MAP_GUARD_TRIGGER,
                KEYPAIR_SLOT_TRUNCATE_TRIGGER,
                KEYPAIR_POOL_TRUNCATE_TRIGGER,
                KEYPAIR_MAP_TRUNCATE_TRIGGER,
            ],
        )
        .await?;
    let Some(row) = existing.next().await? else {
        return Err(KeypoolOwnerError::ExistingAuthority);
    };
    Ok(match (row.get::<bool>(0)?, row.get::<bool>(1)?) {
        (_, true) => KeypairInventory::Enrolled,
        (true, false) => KeypairInventory::LegacySlotsOnly,
        (false, false) => KeypairInventory::Absent,
    })
}

async fn initialize_keypool_owner(
    db: &Database,
    identity: &crate::db::lineage::PgIdentity,
    generation: uuid::Uuid,
    peer_ids: &[PeerId],
    runtime_role: &str,
    activation_delay_ms: i64,
) -> Result<(), KeypoolOwnerError> {
    let mut tx = begin_keypool_owner(db, identity, runtime_role).await?;
    let schema_name = &identity.schema.name.0;
    if keypair_inventory(&mut tx, schema_name).await? != KeypairInventory::Absent {
        return Err(KeypoolOwnerError::ExistingAuthority);
    }
    create_keypool_generation(
        &mut tx,
        schema_name,
        generation,
        peer_ids,
        runtime_role,
        activation_delay_ms,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn prepare_keypool_owner(
    db: &Database,
    identity: &crate::db::lineage::PgIdentity,
    enrollment: &KeypoolEnrollment<'_>,
) -> Result<KeypoolPreparation, KeypoolOwnerError> {
    let delay_ms = i64::try_from(enrollment.activation_delay.as_millis())
        .map_err(|_| KeypoolOwnerError::InvalidPeerOrder)?;
    let mut tx = begin_keypool_owner(db, identity, enrollment.runtime_role).await?;
    let schema_name = &identity.schema.name.0;
    let state = match keypair_inventory(&mut tx, schema_name).await? {
        KeypairInventory::Absent => {
            create_keypool_generation(
                &mut tx,
                schema_name,
                enrollment.generation,
                enrollment.peer_ids,
                enrollment.runtime_role,
                delay_ms,
            )
            .await?;
            KeypoolPreparation::Pending
        }
        KeypairInventory::LegacySlotsOnly => {
            let LegacyKeypairSlots::Retire { legacy_ttl } = enrollment.legacy else {
                return Err(KeypoolOwnerError::LegacyKeypairSlots);
            };
            // A holder that outlives the liveness check below could still
            // believe in its lease for one more lifetime; the pending
            // generation must not activate inside that window.
            if enrollment.activation_delay < legacy_ttl {
                return Err(KeypoolOwnerError::ActivationDelayBelowLegacyTtl);
            }
            retire_legacy_keypair_slots(&mut tx, schema_name, legacy_ttl).await?;
            create_keypool_generation(
                &mut tx,
                schema_name,
                enrollment.generation,
                enrollment.peer_ids,
                enrollment.runtime_role,
                delay_ms,
            )
            .await?;
            KeypoolPreparation::Pending
        }
        KeypairInventory::Enrolled => {
            adopt_enrolled_keypool(&mut tx, schema_name, enrollment).await?
        }
    };
    tx.commit().await?;
    if state == KeypoolPreparation::Active {
        return Ok(state);
    }
    match activate_keypool_owner(db, identity, enrollment.generation).await {
        Ok(()) => Ok(KeypoolPreparation::Active),
        Err(KeypoolOwnerError::ActivationNotReady) => Ok(KeypoolPreparation::Pending),
        Err(error) => Err(error),
    }
}

/// Drop an unenrolled lease table inside the caller's transaction, which
/// then creates the enrolled one. The table must be exactly what the
/// unattested runtime creates and hold no leased row; `DROP TABLE` without
/// `CASCADE` additionally refuses any object that depends on it.
async fn retire_legacy_keypair_slots(
    tx: &mut Transaction<'_>,
    schema_name: &str,
    legacy_ttl: Duration,
) -> Result<(), KeypoolOwnerError> {
    let schema = crate::db::physical_attestation::quote_identifier(schema_name);
    tx.execute(
        &format!("LOCK TABLE {schema}.{KEYPAIR_SLOT_TABLE} IN ACCESS EXCLUSIVE MODE"),
        (),
    )
    .await?;
    let mut shape = tx
        .query(
            "SELECT c.relkind = 'r' AND c.relpersistence = 'p' \
                    AND NOT c.relispartition AND c.relowner = n.nspowner \
                    AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity \
                    AND c.reloftype = 0 \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger tr \
                      WHERE tr.tgrelid = c.oid OR tr.tgconstrrelid = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint con \
                      WHERE (con.conrelid = c.oid AND con.contype <> 'p') \
                         OR con.confrelid = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw \
                      WHERE rw.ev_class = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits inh \
                      WHERE inh.inhrelid = c.oid OR inh.inhparent = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_depend dependent \
                      WHERE dependent.refclassid = 'pg_catalog.pg_class'::regclass \
                        AND dependent.refobjid = c.oid \
                        AND dependent.classid = 'pg_catalog.pg_rewrite'::regclass) \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![schema_name, KEYPAIR_SLOT_TABLE],
        )
        .await?;
    if !matches!(shape.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(KeypoolOwnerError::LegacyShapeMismatch);
    }
    drop(shape);
    let columns = verify_columns_contract(
        tx,
        schema_name,
        KEYPAIR_SLOT_TABLE,
        &[
            ("slot_index", "integer", true, None),
            ("holder_node", "text", false, None),
            ("holder_epoch", "text", false, None),
            ("heartbeat", "timestamp with time zone", true, Some("now()")),
            ("expired", "boolean", true, Some("true")),
        ],
    )
    .await;
    let primary_key = match columns {
        Ok(()) => verify_primary_key(tx, schema_name).await,
        Err(error) => Err(error),
    };
    let index = match primary_key {
        Ok(()) => verify_index_shape(tx, schema_name).await,
        Err(error) => Err(error),
    };
    match index {
        Ok(()) => {}
        Err(LeaseAttachError::Database(error)) => return Err(error.into()),
        Err(_) => return Err(KeypoolOwnerError::LegacyShapeMismatch),
    }
    let ttl_ms = i64::try_from(legacy_ttl.as_millis())
        .map_err(|_| KeypoolOwnerError::ActivationDelayBelowLegacyTtl)?;
    // Liveness is judged on the database clock. The `expired` flag alone is
    // not enough to call a row leased, and a heartbeat ahead of the clock is
    // newer than any cutoff, so it counts as leased.
    let mut live = tx
        .query(
            &format!(
                "SELECT slot_index, holder_node, \
                    (EXTRACT(EPOCH FROM (now() - heartbeat)) * 1000)::bigint \
                 FROM {schema}.{KEYPAIR_SLOT_TABLE} \
                 WHERE holder_node IS NOT NULL AND NOT expired \
                   AND heartbeat > now() - (? || ' milliseconds')::interval \
                 ORDER BY slot_index LIMIT 1"
            ),
            crate::db_params![ttl_ms.to_string()],
        )
        .await?;
    if let Some(row) = live.next().await? {
        return Err(KeypoolOwnerError::LegacyLeaseLive {
            slot: row.get(0)?,
            holder: row.get(1)?,
            age_ms: row.get(2)?,
        });
    }
    drop(live);
    tx.execute(&format!("DROP TABLE {schema}.{KEYPAIR_SLOT_TABLE}"), ())
        .await?;
    Ok(())
}

/// Alter each keypair guard function that carries `SECURITY DEFINER` to
/// `SECURITY INVOKER` and return how many were altered.
async fn convert_definer_guards(
    tx: &mut Transaction<'_>,
    schema_name: &str,
) -> Result<usize, KeypoolOwnerError> {
    let schema = crate::db::physical_attestation::quote_identifier(schema_name);
    let mut definer = tx
        .query(
            "SELECT p.proname::text FROM pg_catalog.pg_proc p \
             JOIN pg_catalog.pg_namespace n ON n.oid = p.pronamespace \
             WHERE n.nspname = ? AND p.proname IN (?, ?, ?, ?) \
               AND p.pronargs = 0 AND p.prosecdef",
            crate::db_params![
                schema_name,
                KEYPAIR_GUARD_FUNCTION,
                KEYPAIR_POOL_GUARD_FUNCTION,
                KEYPAIR_MAP_GUARD_FUNCTION,
                KEYPAIR_TRUNCATE_GUARD_FUNCTION,
            ],
        )
        .await?;
    let mut functions = Vec::new();
    while let Some(row) = definer.next().await? {
        functions.push(row.get::<String>(0)?);
    }
    drop(definer);
    for function in &functions {
        let function = crate::db::physical_attestation::quote_identifier(function);
        tx.execute(
            &format!("ALTER FUNCTION {schema}.{function}() SECURITY INVOKER"),
            (),
        )
        .await?;
    }
    Ok(functions.len())
}

/// Verify an existing enrollment against `enrollment` and report its state.
/// Guard functions that carry `SECURITY DEFINER` are altered to run as
/// the calling role before the exact guard pin is checked, so a database
/// enrolled with definer guards converges on the verified shape.
async fn adopt_enrolled_keypool(
    tx: &mut Transaction<'_>,
    schema_name: &str,
    enrollment: &KeypoolEnrollment<'_>,
) -> Result<KeypoolPreparation, KeypoolOwnerError> {
    let schema = crate::db::physical_attestation::quote_identifier(schema_name);
    convert_definer_guards(tx, schema_name).await?;

    let shape = async {
        verify_table_shape(tx, schema_name).await?;
        verify_columns(tx, schema_name).await?;
        verify_primary_key(tx, schema_name).await?;
        verify_index_shape(tx, schema_name).await?;
        verify_keypool_tables(tx, schema_name).await?;
        verify_keypair_guard(tx, schema_name, Some(enrollment.runtime_role)).await
    };
    match shape.await {
        Ok(()) => {}
        Err(LeaseAttachError::Database(error)) => return Err(error.into()),
        Err(error) => return Err(KeypoolOwnerError::EnrolledShape(error)),
    }

    let mut pool = tx
        .query(
            &format!(
                "SELECT generation::text, state, slot_count \
                 FROM {schema}.{KEYPAIR_POOL_TABLE} WHERE singleton"
            ),
            (),
        )
        .await?;
    let Some(row) = pool.next().await? else {
        return Err(KeypoolOwnerError::EnrollmentMismatch);
    };
    let generation: String = row.get(0)?;
    let state: String = row.get(1)?;
    let slot_count: i32 = row.get(2)?;
    drop(pool);
    if generation != enrollment.generation.to_string()
        || usize::try_from(slot_count).ok() != Some(enrollment.peer_ids.len())
    {
        return Err(KeypoolOwnerError::EnrollmentMismatch);
    }
    let mut slots = tx
        .query(
            &format!(
                "SELECT slot_index, peer_id FROM {schema}.{KEYPAIR_POOL_SLOTS_TABLE} \
                 WHERE generation = ?::uuid ORDER BY slot_index"
            ),
            crate::db_params![generation],
        )
        .await?;
    let mut enrolled = Vec::new();
    while let Some(row) = slots.next().await? {
        let slot_index: i32 = row.get(0)?;
        if usize::try_from(slot_index).ok() != Some(enrolled.len()) {
            return Err(KeypoolOwnerError::EnrollmentMismatch);
        }
        enrolled.push(row.get::<String>(1)?);
    }
    drop(slots);
    let expected = enrollment
        .peer_ids
        .iter()
        .map(PeerId::to_string)
        .collect::<Vec<_>>();
    if enrolled != expected {
        return Err(KeypoolOwnerError::EnrollmentMismatch);
    }
    match state.as_str() {
        "active" => Ok(KeypoolPreparation::Active),
        "pending" => Ok(KeypoolPreparation::Pending),
        _ => Err(KeypoolOwnerError::EnrollmentMismatch),
    }
}

/// Create the enrollment tables, the lease table, their guards and grants,
/// and enroll `peer_ids` as a pending generation. The caller has established
/// that none of these objects exists.
async fn create_keypool_generation(
    tx: &mut Transaction<'_>,
    schema_name: &str,
    generation: uuid::Uuid,
    peer_ids: &[PeerId],
    runtime_role: &str,
    activation_delay_ms: i64,
) -> Result<(), KeypoolOwnerError> {
    let schema = crate::db::physical_attestation::quote_identifier(schema_name);
    tx.execute(
        &format!(
            "CREATE TABLE {schema}.{KEYPAIR_POOL_TABLE} (\
               singleton BOOLEAN PRIMARY KEY CHECK (singleton), \
               generation UUID NOT NULL UNIQUE, \
               state TEXT NOT NULL CHECK (state IN ('pending','active')), \
               slot_count INTEGER NOT NULL CHECK (slot_count > 0), \
               activation_after TIMESTAMPTZ NOT NULL, \
               created_at TIMESTAMPTZ NOT NULL DEFAULT now())"
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "CREATE TABLE {schema}.{KEYPAIR_POOL_SLOTS_TABLE} (\
               generation UUID NOT NULL REFERENCES {schema}.{KEYPAIR_POOL_TABLE}(generation) \
                 ON DELETE RESTRICT, \
               slot_index INTEGER NOT NULL CHECK (slot_index >= 0), \
               peer_id TEXT NOT NULL, \
               PRIMARY KEY (generation, slot_index), \
               UNIQUE (generation, peer_id))"
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "CREATE TABLE {schema}.{KEYPAIR_SLOT_TABLE} (\
               slot_index INTEGER PRIMARY KEY, \
               holder_node TEXT, \
               holder_epoch TEXT, \
               heartbeat TIMESTAMPTZ NOT NULL DEFAULT now(), \
               expired BOOLEAN NOT NULL DEFAULT TRUE, \
               keypool_generation UUID NOT NULL, \
               peer_id TEXT NOT NULL, \
               lease_revision BIGINT NOT NULL DEFAULT 0)"
        ),
        (),
    )
    .await?;

    let function_path = format!("pg_catalog, {schema_name}, pg_temp");
    tx.execute(
        &format!(
            "CREATE FUNCTION {schema}.{KEYPAIR_GUARD_FUNCTION}() RETURNS trigger \
             LANGUAGE plpgsql SECURITY INVOKER SET search_path = {function_path} \
             AS $keypair_guard${KEYPAIR_GUARD_BODY}$keypair_guard$"
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "CREATE TRIGGER {KEYPAIR_GUARD_TRIGGER} BEFORE INSERT OR UPDATE OR DELETE \
             ON {schema}.{KEYPAIR_SLOT_TABLE} FOR EACH ROW \
             EXECUTE FUNCTION {schema}.{KEYPAIR_GUARD_FUNCTION}()"
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "CREATE FUNCTION {schema}.{KEYPAIR_POOL_GUARD_FUNCTION}() RETURNS trigger \
             LANGUAGE plpgsql SECURITY INVOKER SET search_path = {function_path} \
             AS $keypair_pool_guard${KEYPAIR_POOL_GUARD_BODY}$keypair_pool_guard$"
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "CREATE TRIGGER {KEYPAIR_POOL_GUARD_TRIGGER} BEFORE UPDATE OR DELETE \
             ON {schema}.{KEYPAIR_POOL_TABLE} FOR EACH ROW \
             EXECUTE FUNCTION {schema}.{KEYPAIR_POOL_GUARD_FUNCTION}()"
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "CREATE FUNCTION {schema}.{KEYPAIR_MAP_GUARD_FUNCTION}() RETURNS trigger \
             LANGUAGE plpgsql SECURITY INVOKER SET search_path = {function_path} \
             AS $keypair_map_guard${KEYPAIR_MAP_GUARD_BODY}$keypair_map_guard$"
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "CREATE TRIGGER {KEYPAIR_MAP_GUARD_TRIGGER} BEFORE INSERT OR UPDATE OR DELETE \
             ON {schema}.{KEYPAIR_POOL_SLOTS_TABLE} FOR EACH ROW \
             EXECUTE FUNCTION {schema}.{KEYPAIR_MAP_GUARD_FUNCTION}()"
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "CREATE FUNCTION {schema}.{KEYPAIR_TRUNCATE_GUARD_FUNCTION}() RETURNS trigger \
             LANGUAGE plpgsql SECURITY INVOKER SET search_path = {function_path} \
             AS $keypair_truncate_guard${KEYPAIR_TRUNCATE_GUARD_BODY}$keypair_truncate_guard$"
        ),
        (),
    )
    .await?;
    for (table, trigger) in [
        (KEYPAIR_SLOT_TABLE, KEYPAIR_SLOT_TRUNCATE_TRIGGER),
        (KEYPAIR_POOL_TABLE, KEYPAIR_POOL_TRUNCATE_TRIGGER),
        (KEYPAIR_POOL_SLOTS_TABLE, KEYPAIR_MAP_TRUNCATE_TRIGGER),
    ] {
        tx.execute(
            &format!(
                "CREATE TRIGGER {trigger} BEFORE TRUNCATE ON {schema}.{table} \
                 FOR EACH STATEMENT EXECUTE FUNCTION {schema}.{KEYPAIR_TRUNCATE_GUARD_FUNCTION}()"
            ),
            (),
        )
        .await?;
    }

    tx.execute(
        &format!(
            "REVOKE ALL ON TABLE {schema}.{KEYPAIR_POOL_TABLE}, \
             {schema}.{KEYPAIR_POOL_SLOTS_TABLE}, {schema}.{KEYPAIR_SLOT_TABLE} FROM PUBLIC"
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "REVOKE ALL ON FUNCTION {schema}.{KEYPAIR_GUARD_FUNCTION}(), \
             {schema}.{KEYPAIR_POOL_GUARD_FUNCTION}(), \
             {schema}.{KEYPAIR_MAP_GUARD_FUNCTION}(), \
             {schema}.{KEYPAIR_TRUNCATE_GUARD_FUNCTION}() FROM PUBLIC"
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!("GRANT USAGE ON SCHEMA {schema} TO {runtime_role}"),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "GRANT SELECT ON TABLE {schema}.{KEYPAIR_POOL_TABLE}, \
             {schema}.{KEYPAIR_POOL_SLOTS_TABLE} TO {runtime_role}"
        ),
        (),
    )
    .await?;
    tx.execute(
        &format!(
            "GRANT SELECT, INSERT, UPDATE ON TABLE {schema}.{KEYPAIR_SLOT_TABLE} \
             TO {runtime_role}"
        ),
        (),
    )
    .await?;

    tx.execute(
        &format!(
            "INSERT INTO {schema}.{KEYPAIR_POOL_TABLE} \
             (singleton, generation, state, slot_count, activation_after) \
             VALUES (TRUE, ?::uuid, 'pending', ?, now() + (? || ' milliseconds')::interval)"
        ),
        crate::db_params![
            generation.to_string(),
            i32::try_from(peer_ids.len()).map_err(|_| KeypoolOwnerError::InvalidPeerOrder)?,
            activation_delay_ms.to_string()
        ],
    )
    .await?;
    for (slot_index, peer_id) in peer_ids.iter().enumerate() {
        let slot_index =
            i32::try_from(slot_index).map_err(|_| KeypoolOwnerError::InvalidPeerOrder)?;
        tx.execute(
            &format!(
                "INSERT INTO {schema}.{KEYPAIR_POOL_SLOTS_TABLE} \
                 (generation, slot_index, peer_id) VALUES (?::uuid, ?, ?)"
            ),
            crate::db_params![generation.to_string(), slot_index, peer_id.to_string()],
        )
        .await?;
    }
    Ok(())
}

async fn activate_keypool_owner(
    db: &Database,
    identity: &crate::db::lineage::PgIdentity,
    generation: uuid::Uuid,
) -> Result<(), KeypoolOwnerError> {
    let mut tx = db.begin().await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE",
        (),
    )
    .await?;
    tx.query(
        "SELECT pg_catalog.pg_advisory_xact_lock(?)",
        crate::db_params![KEYPAIR_OWNER_ADVISORY_LOCK],
    )
    .await?;
    crate::room_effect_outbox::pin_room_owner_schema(&mut tx, identity)
        .await
        .map_err(|_| KeypoolOwnerError::UnsafeOwnerBoundary)?;
    let schema = crate::db::physical_attestation::quote_identifier(&identity.schema.name.0);
    tx.execute(
        &format!(
            "LOCK TABLE {schema}.{KEYPAIR_SLOT_TABLE}, {schema}.{KEYPAIR_POOL_SLOTS_TABLE}, \
             {schema}.{KEYPAIR_POOL_TABLE} IN ACCESS EXCLUSIVE MODE"
        ),
        (),
    )
    .await?;
    let mut rows = tx
        .query(
            &format!(
                "SELECT state, activation_after <= now(), \
                    slot_count = (SELECT count(*) FROM {schema}.{KEYPAIR_POOL_SLOTS_TABLE} \
                      WHERE generation = {schema}.{KEYPAIR_POOL_TABLE}.generation) \
                    AND (SELECT min(slot_index) = 0 AND max(slot_index) = slot_count - 1 \
                         FROM {schema}.{KEYPAIR_POOL_SLOTS_TABLE} \
                         WHERE generation = {schema}.{KEYPAIR_POOL_TABLE}.generation), \
                    NOT EXISTS (SELECT 1 FROM {schema}.{KEYPAIR_SLOT_TABLE}) \
                 FROM {schema}.{KEYPAIR_POOL_TABLE} \
                 WHERE singleton AND generation = ?::uuid"
            ),
            crate::db_params![generation.to_string()],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(KeypoolOwnerError::InvalidGenerationState);
    };
    let state: String = row.get(0)?;
    if state != "pending" {
        return Err(KeypoolOwnerError::InvalidGenerationState);
    }
    let ready = row.get::<bool>(1)? && row.get::<bool>(2)? && row.get::<bool>(3)?;
    if !ready {
        return Err(KeypoolOwnerError::ActivationNotReady);
    }
    let mut row_count = tx
        .query(
            &format!("SELECT count(*)::bigint FROM {schema}.{KEYPAIR_SLOT_TABLE}"),
            (),
        )
        .await?;
    if !matches!(row_count.next().await?, Some(row) if row.get::<i64>(0)? == 0) {
        return Err(KeypoolOwnerError::ActivationNotReady);
    }
    tx.execute(
        &format!(
            "UPDATE {schema}.{KEYPAIR_POOL_TABLE} SET state = 'active' \
             WHERE singleton AND generation = ?::uuid AND state = 'pending' \
               AND activation_after <= now()"
        ),
        crate::db_params![generation.to_string()],
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

async fn verify_keypool_runtime_candidate(
    tx: &mut Transaction<'_>,
    schema: &str,
    runtime_role: &str,
) -> Result<(), KeypoolOwnerError> {
    let mut rows = tx
        .query(
            "SELECT runtime.rolcanlogin AND NOT runtime.rolsuper \
               AND NOT runtime.rolcreatedb AND NOT runtime.rolcreaterole \
               AND NOT runtime.rolreplication AND NOT runtime.rolbypassrls \
               AND runtime.oid <> database.datdba AND runtime.oid <> ns.nspowner \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m \
                 WHERE m.member = runtime.oid OR m.roleid = runtime.oid) \
               AND pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'USAGE') \
               AND NOT pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'CREATE') \
               AND NOT pg_catalog.has_database_privilege(runtime.oid, database.oid, 'CREATE,TEMP') \
             FROM pg_catalog.pg_roles runtime \
             JOIN pg_catalog.pg_namespace ns ON ns.nspname = ? \
             JOIN pg_catalog.pg_database database \
               ON database.datname = pg_catalog.current_database() \
             WHERE runtime.rolname = ?",
            crate::db_params![schema, runtime_role],
        )
        .await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(KeypoolOwnerError::UnsafeOwnerBoundary);
    }
    Ok(())
}

fn validate_peer_order(peer_ids: &[PeerId]) -> Result<(), ()> {
    if peer_ids.is_empty() {
        return Err(());
    }
    let mut unique = peer_ids.to_vec();
    unique.sort_unstable();
    unique.dedup();
    if unique.len() != peer_ids.len() {
        return Err(());
    }
    Ok(())
}

fn configured_peer_order(entries: &[String]) -> Result<Vec<PeerId>, LeaseAttachError> {
    if entries.is_empty() {
        return Err(LeaseAttachError::InvalidConfiguredKeypairPool);
    }
    let peer_ids = entries
        .iter()
        .map(|entry| {
            crate::clustering::identity::keypair_from_pool_entry(entry)
                .map(|keypair| keypair.public().to_peer_id())
                .map_err(|_| LeaseAttachError::InvalidConfiguredKeypairPool)
        })
        .collect::<Result<Vec<_>, _>>()?;
    validate_peer_order(&peer_ids).map_err(|_| LeaseAttachError::InvalidConfiguredKeypairPool)?;
    Ok(peer_ids)
}

async fn verify_attested_keypair_slot_pair(
    db: &Database,
    main_capability: &AttestedPostgresWriterCapability,
    control_plane: &Database,
    control_plane_capability: &AttestedPostgresWriterCapability,
    expected_peer_ids: &[PeerId],
) -> Result<EnrolledKeypairPool, LeaseAttachError> {
    if validate_peer_order(expected_peer_ids).is_err() {
        return Err(LeaseAttachError::KeypairPoolOrderMismatch);
    }
    if db.driver() != DatabaseDriver::Postgres {
        return Err(LeaseAttachError::PostgresRequired);
    }
    let owned_capability = db
        .attested_pg_writer_capability()
        .ok_or(LeaseAttachError::AttestedMainPoolRequired)?;
    db.verify_attested_pg_writer_capability(&owned_capability)
        .map_err(DatabaseError::from)?;
    if !owned_capability.belongs_to(main_capability) {
        return Err(LeaseAttachError::AttestedMainPoolRequired);
    }
    let owned_control_capability = control_plane
        .attested_pg_writer_capability()
        .ok_or(LeaseAttachError::AttestedControlPlanePoolRequired)?;
    control_plane
        .verify_attested_pg_writer_capability(&owned_control_capability)
        .map_err(DatabaseError::from)?;
    if !owned_control_capability.belongs_to(control_plane_capability) {
        return Err(LeaseAttachError::AttestedControlPlanePoolRequired);
    }

    let identity = main_capability.identity();
    let schema = &identity.schema.name.0;
    crate::db::physical_attestation::validate_identifier(schema, "keypair-slot schema")
        .map_err(|_| LeaseAttachError::InvalidSchema)?;
    let qualified_table = format!(
        "{}.{}",
        crate::db::physical_attestation::quote_identifier(schema),
        crate::db::physical_attestation::quote_identifier(KEYPAIR_SLOT_TABLE)
    );
    let qualified_keypool = format!(
        "{}.{}",
        crate::db::physical_attestation::quote_identifier(schema),
        crate::db::physical_attestation::quote_identifier(KEYPAIR_POOL_TABLE)
    );
    let qualified_keypool_slots = format!(
        "{}.{}",
        crate::db::physical_attestation::quote_identifier(schema),
        crate::db::physical_attestation::quote_identifier(KEYPAIR_POOL_SLOTS_TABLE)
    );

    let mut main_tx = begin_attested_check(
        db,
        main_capability,
        &[
            &qualified_table,
            &qualified_keypool,
            &qualified_keypool_slots,
        ],
    )
    .await?;
    let mut control_tx = begin_attested_check(
        control_plane,
        control_plane_capability,
        &[
            &qualified_table,
            &qualified_keypool,
            &qualified_keypool_slots,
        ],
    )
    .await?;

    // Both pools now hold ACCESS SHARE against the same qualified relation.
    // The compatible locks keep owner DDL from changing the table between
    // their independent catalog snapshots.
    let main_pool = verify_keypair_slot_snapshot(
        &mut main_tx,
        schema,
        i64::from(identity.schema.oid),
        expected_peer_ids,
    )
    .await?;
    let control_pool = verify_keypair_slot_snapshot(
        &mut control_tx,
        schema,
        i64::from(identity.schema.oid),
        expected_peer_ids,
    )
    .await?;
    if main_pool.generation != control_pool.generation
        || main_pool.peer_ids != control_pool.peer_ids
    {
        return Err(LeaseAttachError::PhysicalIdentityMismatch);
    }
    control_tx.rollback().await?;
    main_tx.rollback().await?;
    Ok(main_pool)
}

async fn begin_attested_check<'a>(
    db: &'a Database,
    capability: &AttestedPostgresWriterCapability,
    qualified_tables: &[&str],
) -> Result<Transaction<'a>, LeaseAttachError> {
    let mut tx = db.begin().await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ READ ONLY",
        (),
    )
    .await?;
    crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
        .await
        .map_err(|error| match error {
            crate::db::read_only_timeout::ReadOnlyTimeoutError::InvalidSetting { setting } => {
                LeaseAttachError::InvalidTimeoutSetting { setting }
            }
            crate::db::read_only_timeout::ReadOnlyTimeoutError::Database(error) => {
                LeaseAttachError::Database(error)
            }
        })?;
    for table in qualified_tables {
        tx.execute(&format!("LOCK TABLE {table} IN ACCESS SHARE MODE"), ())
            .await
            .map_err(|_| LeaseAttachError::InvalidSchemaShape)?;
    }

    capability.attest_transaction(&mut tx).await?;
    Ok(tx)
}

async fn verify_keypair_slot_snapshot(
    tx: &mut Transaction<'_>,
    schema: &str,
    schema_oid: i64,
    expected_peer_ids: &[PeerId],
) -> Result<EnrolledKeypairPool, LeaseAttachError> {
    verify_active_schema(tx, schema, schema_oid).await?;
    verify_table_shape(tx, schema).await?;
    verify_columns(tx, schema).await?;
    verify_primary_key(tx, schema).await?;
    verify_index_shape(tx, schema).await?;
    verify_runtime_acl(tx, schema).await?;
    verify_keypool_tables(tx, schema).await?;
    verify_keypool_acl(tx, schema).await?;
    verify_keypair_guard(tx, schema, None).await?;
    read_enrolled_peer_order(tx, schema, expected_peer_ids).await
}

async fn verify_active_schema(
    tx: &mut Transaction<'_>,
    schema: &str,
    schema_oid: i64,
) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.current_schema()::text, n.oid::bigint \
             FROM pg_catalog.pg_namespace n WHERE n.nspname = ?",
            crate::db_params![schema],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LeaseAttachError::ActiveSchemaMismatch);
    };
    let active: Option<String> = row.get(0)?;
    let actual_oid: i64 = row.get(1)?;
    if active.as_deref() != Some(schema) || actual_oid != schema_oid {
        return Err(LeaseAttachError::ActiveSchemaMismatch);
    }
    Ok(())
}

async fn verify_table_shape(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            "SELECT c.relkind = 'r' AND c.relpersistence = 'p' \
                    AND NOT c.relispartition AND c.relowner = n.nspowner \
                    AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity \
                    AND c.reloftype = 0 \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint con \
                                    WHERE con.conrelid = c.oid AND con.contype <> 'p') \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint inbound \
                                    WHERE inbound.confrelid = c.oid AND inbound.contype = 'f') \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger tr \
                                    WHERE (tr.tgrelid = c.oid OR tr.tgconstrrelid = c.oid) \
                                      AND tr.tgname NOT IN (?, ?)) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw \
                                    WHERE rw.ev_class = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits inh \
                                    WHERE inh.inhrelid = c.oid OR inh.inhparent = c.oid) \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![
                KEYPAIR_GUARD_TRIGGER,
                KEYPAIR_SLOT_TRUNCATE_TRIGGER,
                schema,
                KEYPAIR_SLOT_TABLE,
            ],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LeaseAttachError::InvalidSchemaShape);
    };
    if !row.get::<bool>(0)? {
        return Err(LeaseAttachError::InvalidSchemaShape);
    }
    Ok(())
}

async fn verify_columns(tx: &mut Transaction<'_>, schema: &str) -> Result<(), LeaseAttachError> {
    verify_columns_contract(
        tx,
        schema,
        KEYPAIR_SLOT_TABLE,
        &[
            ("slot_index", "integer", true, None),
            ("holder_node", "text", false, None),
            ("holder_epoch", "text", false, None),
            ("heartbeat", "timestamp with time zone", true, Some("now()")),
            ("expired", "boolean", true, Some("true")),
            ("keypool_generation", "uuid", true, None),
            ("peer_id", "text", true, None),
            ("lease_revision", "bigint", true, Some("0")),
        ],
    )
    .await
}

type KeypairColumnContract = (&'static str, &'static str, bool, Option<&'static str>);

async fn verify_columns_contract(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &str,
    expected: &[KeypairColumnContract],
) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), \
                    a.attnotnull, a.attgenerated::text, a.attidentity::text, \
                    pg_catalog.pg_get_expr(d.adbin, d.adrelid), \
                    a.attcollation = typ.typcollation \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             JOIN pg_catalog.pg_type typ ON typ.oid = a.atttypid \
             LEFT JOIN pg_catalog.pg_attrdef d \
               ON d.adrelid = a.attrelid AND d.adnum = a.attnum \
             WHERE n.nspname = ? AND c.relname = ? AND c.relkind = 'r' \
               AND a.attnum > 0 AND NOT a.attisdropped",
            crate::db_params![schema, table],
        )
        .await?;
    let mut actual = HashMap::new();
    while let Some(row) = rows.next().await? {
        actual.insert(
            row.get::<String>(0)?,
            (
                row.get::<String>(1)?,
                row.get::<bool>(2)?,
                row.get::<String>(3)?,
                row.get::<String>(4)?,
                row.get::<Option<String>>(5)?,
                row.get::<bool>(6)?,
            ),
        );
    }
    if actual.len() != expected.len() {
        return Err(LeaseAttachError::InvalidSchemaShape);
    }
    for (name, kind, not_null, default) in expected {
        let Some((actual_kind, actual_not_null, generated, identity, actual_default, collation)) =
            actual.get(*name)
        else {
            return Err(LeaseAttachError::InvalidSchemaShape);
        };
        if actual_kind != kind
            || *actual_not_null != *not_null
            || !generated.is_empty()
            || !identity.is_empty()
            || actual_default.as_deref() != *default
            || !collation
        {
            return Err(LeaseAttachError::InvalidSchemaShape);
        }
    }
    Ok(())
}

async fn verify_primary_key(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            "SELECT a.attname::text FROM pg_catalog.pg_constraint c \
             JOIN pg_catalog.pg_class t ON t.oid = c.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             CROSS JOIN LATERAL pg_catalog.unnest(c.conkey) \
               WITH ORDINALITY k(attnum, position) \
             JOIN pg_catalog.pg_attribute a ON a.attrelid = t.oid AND a.attnum = k.attnum \
             WHERE c.contype = 'p' AND NOT c.condeferrable AND NOT c.condeferred \
               AND c.convalidated \
               AND EXISTS (SELECT 1 FROM pg_catalog.pg_index i \
                           WHERE i.indexrelid = c.conindid AND i.indisvalid \
                             AND i.indisready AND i.indislive AND i.indimmediate) \
               AND n.nspname = ? AND t.relname = ? ORDER BY k.position",
            crate::db_params![schema, KEYPAIR_SLOT_TABLE],
        )
        .await?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await? {
        columns.push(row.get::<String>(0)?);
    }
    if columns.iter().map(String::as_str).collect::<Vec<_>>() != ["slot_index"] {
        return Err(LeaseAttachError::InvalidSchemaShape);
    }
    Ok(())
}

async fn verify_index_shape(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.count(*)::bigint, \
                    COALESCE(pg_catalog.bool_and( \
                      i.indisprimary AND i.indisunique AND i.indisvalid AND i.indisready \
                      AND i.indislive AND i.indimmediate AND i.indpred IS NULL \
                      AND i.indexprs IS NULL AND am.amname = 'btree' \
                      AND i.indnkeyatts = 1 AND i.indnatts = 1 \
                      AND pg_catalog.array_to_string(ARRAY( \
                        SELECT a.attname::text \
                        FROM pg_catalog.unnest(i.indkey) WITH ORDINALITY k(attnum, position) \
                        JOIN pg_catalog.pg_attribute a \
                          ON a.attrelid = t.oid AND a.attnum = k.attnum \
                        WHERE k.position <= i.indnkeyatts ORDER BY k.position), ',') = 'slot_index' \
                      AND (SELECT COALESCE(pg_catalog.bool_and(k.option = 0), true) \
                           FROM pg_catalog.unnest(i.indoption) \
                             WITH ORDINALITY k(option, position) \
                           WHERE k.position <= i.indnkeyatts) \
                      AND NOT EXISTS ( \
                        SELECT 1 \
                        FROM pg_catalog.unnest(i.indclass) WITH ORDINALITY ic(opclass_oid, position) \
                        JOIN pg_catalog.pg_attribute a \
                          ON a.attrelid = t.oid AND a.attname = 'slot_index' \
                        JOIN pg_catalog.pg_opclass opc ON opc.oid = ic.opclass_oid \
                        JOIN pg_catalog.pg_am method ON method.oid = opc.opcmethod \
                        WHERE NOT opc.opcdefault OR method.amname <> 'btree' \
                          OR opc.opcintype <> a.atttypid) \
                      AND NOT EXISTS ( \
                        SELECT 1 \
                        FROM pg_catalog.unnest(i.indcollation) WITH ORDINALITY ic(collation_oid, position) \
                        JOIN pg_catalog.pg_attribute a \
                          ON a.attrelid = t.oid AND a.attname = 'slot_index' \
                        WHERE ic.collation_oid <> a.attcollation)), false) \
             FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class t ON t.oid = i.indrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = t.relnamespace \
             JOIN pg_catalog.pg_class ix ON ix.oid = i.indexrelid \
             JOIN pg_catalog.pg_am am ON am.oid = ix.relam \
             WHERE n.nspname = ? AND t.relname = ?",
            crate::db_params![schema, KEYPAIR_SLOT_TABLE],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or(LeaseAttachError::InvalidSchemaShape)?;
    let index_count: i64 = row.get(0)?;
    let valid_primary_index: bool = row.get(1)?;
    if index_count != 1 || !valid_primary_index {
        return Err(LeaseAttachError::InvalidSchemaShape);
    }
    Ok(())
}

async fn verify_runtime_acl(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            r#"SELECT runtime.rolcanlogin AND NOT runtime.rolsuper
                    AND NOT runtime.rolcreaterole AND NOT runtime.rolcreatedb
                    AND NOT runtime.rolreplication AND NOT runtime.rolbypassrls
                    AND runtime.oid <> database.datdba
                    AND runtime.oid <> owner.oid AND NOT owner.rolcanlogin
                    AND owner.oid <> database.datdba
                    AND NOT owner.rolsuper AND NOT owner.rolcreaterole
                    AND NOT owner.rolcreatedb AND NOT owner.rolreplication
                    AND NOT owner.rolbypassrls
                    AND session_user::text = runtime.rolname
                    AND current_user::text = runtime.rolname
                    AND NOT pg_catalog.has_database_privilege(
                      runtime.oid, database.oid, 'CREATE,TEMP')
                    AND NOT pg_catalog.has_parameter_privilege(
                      runtime.oid, 'session_replication_role', 'SET')
                    AND pg_catalog.current_setting('session_replication_role') = 'origin'
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members m
                                    WHERE m.member = runtime.oid OR m.roleid = runtime.oid)
                    AND pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'USAGE')
                    AND NOT pg_catalog.has_schema_privilege(runtime.oid, ns.oid, 'CREATE')
                    AND ns.nspowner = owner.oid AND relation.relowner = owner.oid
                    AND NOT EXISTS (
                      SELECT 1 FROM pg_catalog.aclexplode(
                        COALESCE(ns.nspacl, pg_catalog.acldefault('n', ns.nspowner))
                      ) acl WHERE acl.grantee = 0
                        OR (acl.grantee <> owner.oid AND (
                          acl.is_grantable OR acl.privilege_type <> 'USAGE'))
                    )
                    AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'SELECT')
                    AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'INSERT')
                    AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'UPDATE')
                    AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'DELETE')
                    AND NOT pg_catalog.has_table_privilege(
                      runtime.oid, relation.oid, 'TRUNCATE, REFERENCES, TRIGGER')
                    AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000
                      THEN NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'MAINTAIN')
                      ELSE true END
                    AND NOT pg_catalog.has_table_privilege(
                      runtime.oid, relation.oid,
                      'SELECT WITH GRANT OPTION, INSERT WITH GRANT OPTION,
                       UPDATE WITH GRANT OPTION, DELETE WITH GRANT OPTION,
                       TRUNCATE WITH GRANT OPTION, REFERENCES WITH GRANT OPTION,
                       TRIGGER WITH GRANT OPTION')
                    AND NOT EXISTS (
                      SELECT 1 FROM pg_catalog.aclexplode(
                        COALESCE(relation.relacl, pg_catalog.acldefault('r', relation.relowner))
                      ) acl WHERE acl.grantee NOT IN (owner.oid, runtime.oid)
                        OR (acl.grantee = runtime.oid AND (
                          acl.is_grantable OR acl.privilege_type NOT IN
                            ('SELECT', 'INSERT', 'UPDATE')))
                    )
                    AND NOT EXISTS (
                      SELECT 1 FROM pg_catalog.pg_attribute a
                      WHERE a.attrelid = relation.oid AND a.attnum > 0
                        AND NOT a.attisdropped AND a.attacl IS NOT NULL
                    )
             FROM pg_catalog.pg_roles runtime
             JOIN pg_catalog.pg_class relation ON relation.relname = ?
             JOIN pg_catalog.pg_namespace ns ON ns.oid = relation.relnamespace
             JOIN pg_catalog.pg_roles owner ON owner.oid = relation.relowner
             JOIN pg_catalog.pg_database database
               ON database.datname = pg_catalog.current_database()
             WHERE runtime.rolname = current_user AND ns.nspname = ?"#,
            crate::db_params![KEYPAIR_SLOT_TABLE, schema],
        )
        .await?;
    let safe = match rows.next().await? {
        Some(row) => row.get::<bool>(0)?,
        None => false,
    };
    if !safe {
        return Err(LeaseAttachError::UnsafeRuntimeRole);
    }
    Ok(())
}

async fn verify_keypool_tables(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), LeaseAttachError> {
    verify_aux_table_shape(
        tx,
        schema,
        KEYPAIR_POOL_TABLE,
        KEYPAIR_POOL_GUARD_TRIGGER,
        KEYPAIR_POOL_TRUNCATE_TRIGGER,
    )
    .await?;
    verify_aux_table_shape(
        tx,
        schema,
        KEYPAIR_POOL_SLOTS_TABLE,
        KEYPAIR_MAP_GUARD_TRIGGER,
        KEYPAIR_MAP_TRUNCATE_TRIGGER,
    )
    .await?;
    verify_columns_contract(
        tx,
        schema,
        KEYPAIR_POOL_TABLE,
        &[
            ("singleton", "boolean", true, None),
            ("generation", "uuid", true, None),
            ("state", "text", true, None),
            ("slot_count", "integer", true, None),
            ("activation_after", "timestamp with time zone", true, None),
            (
                "created_at",
                "timestamp with time zone",
                true,
                Some("now()"),
            ),
        ],
    )
    .await?;
    verify_columns_contract(
        tx,
        schema,
        KEYPAIR_POOL_SLOTS_TABLE,
        &[
            ("generation", "uuid", true, None),
            ("slot_index", "integer", true, None),
            ("peer_id", "text", true, None),
        ],
    )
    .await?;
    verify_keypool_constraints(tx, schema, KEYPAIR_POOL_TABLE).await?;
    verify_keypool_constraints(tx, schema, KEYPAIR_POOL_SLOTS_TABLE).await?;
    Ok(())
}

async fn verify_aux_table_shape(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &str,
    expected_trigger: &str,
    expected_truncate_trigger: &str,
) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            "SELECT c.relkind = 'r' AND c.relpersistence = 'p' \
                    AND NOT c.relispartition AND c.relowner = n.nspowner \
                    AND NOT c.relrowsecurity AND NOT c.relforcerowsecurity \
                    AND c.reloftype = 0 \
                    AND (SELECT count(*) = 2 FROM pg_catalog.pg_trigger tr \
                         WHERE tr.tgrelid = c.oid AND NOT tr.tgisinternal \
                           AND tr.tgname IN (?, ?)) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger tr \
                                    WHERE (tr.tgrelid = c.oid OR tr.tgconstrrelid = c.oid) \
                                      AND NOT tr.tgisinternal \
                                      AND NOT (tr.tgrelid = c.oid \
                                        AND tr.tgname IN (?, ?))) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger tr \
                                    WHERE (tr.tgrelid = c.oid OR tr.tgconstrrelid = c.oid) \
                                      AND tr.tgisinternal AND NOT EXISTS ( \
                                        SELECT 1 FROM pg_catalog.pg_constraint fk \
                                        JOIN pg_catalog.pg_class child \
                                          ON child.oid = fk.conrelid \
                                        JOIN pg_catalog.pg_namespace child_ns \
                                          ON child_ns.oid = child.relnamespace \
                                        JOIN pg_catalog.pg_class parent \
                                          ON parent.oid = fk.confrelid \
                                        JOIN pg_catalog.pg_namespace parent_ns \
                                          ON parent_ns.oid = parent.relnamespace \
                                        WHERE fk.oid = tr.tgconstraint AND fk.contype = 'f' \
                                          AND child_ns.nspname = ? \
                                          AND child.relname = ? \
                                          AND parent_ns.nspname = ? \
                                          AND parent.relname = ?)) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint inbound \
                                    WHERE inbound.contype = 'f' AND inbound.confrelid = c.oid \
                                      AND NOT (c.relname = ? \
                                        AND inbound.conrelid = (SELECT child.oid \
                                          FROM pg_catalog.pg_class child \
                                          JOIN pg_catalog.pg_namespace child_ns \
                                            ON child_ns.oid = child.relnamespace \
                                          WHERE child_ns.nspname = ? \
                                            AND child.relname = ?))) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rw \
                                    WHERE rw.ev_class = c.oid) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits inh \
                                    WHERE inh.inhrelid = c.oid OR inh.inhparent = c.oid) \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![
                expected_trigger,
                expected_truncate_trigger,
                expected_trigger,
                expected_truncate_trigger,
                schema,
                KEYPAIR_POOL_SLOTS_TABLE,
                schema,
                KEYPAIR_POOL_TABLE,
                KEYPAIR_POOL_TABLE,
                schema,
                KEYPAIR_POOL_SLOTS_TABLE,
                schema,
                table,
            ],
        )
        .await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(LeaseAttachError::InvalidKeypairPoolShape);
    }
    Ok(())
}

async fn verify_keypool_constraints(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &str,
) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            "SELECT con.contype::text, \
                    pg_catalog.array_to_string(ARRAY( \
                      SELECT a.attname::text \
                      FROM pg_catalog.unnest(con.conkey) WITH ORDINALITY k(attnum, position) \
                      JOIN pg_catalog.pg_attribute a \
                        ON a.attrelid = con.conrelid AND a.attnum = k.attnum \
                      ORDER BY k.position), ','), \
                    COALESCE(ref.relname, ''), \
                    pg_catalog.array_to_string(ARRAY( \
                      SELECT a.attname::text \
                      FROM pg_catalog.unnest(con.confkey) WITH ORDINALITY k(attnum, position) \
                      JOIN pg_catalog.pg_attribute a \
                        ON a.attrelid = con.confrelid AND a.attnum = k.attnum \
                      ORDER BY k.position), ','), \
                    con.condeferrable, con.condeferred, con.convalidated, \
                    CASE WHEN con.contype IN ('p', 'u') THEN \
                      COALESCE(ix.indisvalid AND ix.indisready AND ix.indislive \
                        AND ix.indimmediate AND ix.indisunique \
                        AND ix.indisprimary = (con.contype = 'p') \
                        AND ix.indpred IS NULL AND ix.indexprs IS NULL \
                        AND ix.indnkeyatts = pg_catalog.cardinality(con.conkey) \
                        AND ix.indnatts = ix.indnkeyatts \
                        AND am.amname = 'btree' \
                        AND pg_catalog.array_to_string(ARRAY( \
                          SELECT a.attname::text \
                          FROM pg_catalog.unnest(ix.indkey) WITH ORDINALITY k(attnum, position) \
                          JOIN pg_catalog.pg_attribute a \
                            ON a.attrelid = con.conrelid AND a.attnum = k.attnum \
                          WHERE k.position <= ix.indnkeyatts ORDER BY k.position), ',') \
                          = pg_catalog.array_to_string(ARRAY( \
                            SELECT a.attname::text \
                            FROM pg_catalog.unnest(con.conkey) WITH ORDINALITY k(attnum, position) \
                            JOIN pg_catalog.pg_attribute a \
                              ON a.attrelid = con.conrelid AND a.attnum = k.attnum \
                            ORDER BY k.position), ',') \
                        AND NOT EXISTS ( \
                          SELECT 1 \
                          FROM pg_catalog.unnest(ix.indclass) WITH ORDINALITY ic(opclass_oid, position) \
                          JOIN pg_catalog.unnest(ix.indkey) WITH ORDINALITY ik(attnum, position) \
                            ON ik.position = ic.position \
                          JOIN pg_catalog.pg_attribute a \
                            ON a.attrelid = con.conrelid AND a.attnum = ik.attnum \
                          JOIN pg_catalog.pg_opclass opc ON opc.oid = ic.opclass_oid \
                          JOIN pg_catalog.pg_am amethod ON amethod.oid = opc.opcmethod \
                          WHERE ic.position <= ix.indnkeyatts \
                            AND (NOT opc.opcdefault OR amethod.amname <> 'btree' \
                              OR NOT EXISTS (SELECT 1 FROM pg_catalog.pg_opclass def \
                                WHERE def.opcmethod = opc.opcmethod \
                                  AND def.opcintype = a.atttypid AND def.opcdefault \
                                  AND def.oid = opc.oid)) \
                        ) \
                        AND NOT EXISTS ( \
                          SELECT 1 \
                          FROM pg_catalog.unnest(ix.indcollation) WITH ORDINALITY ic(collation_oid, position) \
                          JOIN pg_catalog.unnest(ix.indkey) WITH ORDINALITY ik(attnum, position) \
                            ON ik.position = ic.position \
                          JOIN pg_catalog.pg_attribute a \
                            ON a.attrelid = con.conrelid AND a.attnum = ik.attnum \
                          WHERE ic.position <= ix.indnkeyatts \
                            AND ic.collation_oid <> a.attcollation \
                        ), false) \
                    ELSE con.contype = 'f' END, \
                    con.confupdtype::text, con.confdeltype::text, con.confmatchtype::text, \
                    pg_catalog.pg_get_constraintdef(con.oid), \
                    (con.contype <> 'f' OR ref.relnamespace = n.oid) \
             FROM pg_catalog.pg_constraint con \
             JOIN pg_catalog.pg_class rel ON rel.oid = con.conrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = rel.relnamespace \
             LEFT JOIN pg_catalog.pg_class ref ON ref.oid = con.confrelid \
             LEFT JOIN pg_catalog.pg_index ix ON ix.indexrelid = con.conindid \
             LEFT JOIN pg_catalog.pg_class ixrel ON ixrel.oid = ix.indexrelid \
             LEFT JOIN pg_catalog.pg_am am ON am.oid = ixrel.relam \
             WHERE n.nspname = ? AND rel.relname = ?",
            crate::db_params![schema, table],
        )
        .await?;
    let mut actual = Vec::new();
    while let Some(row) = rows.next().await? {
        actual.push((
            row.get::<String>(0)?,
            row.get::<String>(1)?,
            row.get::<String>(2)?,
            row.get::<String>(3)?,
            row.get::<bool>(4)?,
            row.get::<bool>(5)?,
            row.get::<bool>(6)?,
            row.get::<bool>(7)?,
            row.get::<String>(8)?,
            row.get::<String>(9)?,
            row.get::<String>(10)?,
            row.get::<String>(11)?,
            row.get::<bool>(12)?,
        ));
    }
    let mut expected = if table == KEYPAIR_POOL_TABLE {
        vec![
            ("p", "singleton", "", "", "", "PRIMARY KEY (singleton)"),
            ("u", "generation", "", "", "", "UNIQUE (generation)"),
            ("c", "singleton", "", "", "", "CHECK (singleton)"),
            ("c", "slot_count", "", "", "", "CHECK ((slot_count > 0))"),
            (
                "c",
                "state",
                "",
                "",
                "",
                "CHECK ((state = ANY (ARRAY['pending'::text, 'active'::text])))",
            ),
        ]
    } else if table == KEYPAIR_POOL_SLOTS_TABLE {
        vec![
            (
                "p",
                "generation,slot_index",
                "",
                "",
                "",
                "PRIMARY KEY (generation, slot_index)",
            ),
            (
                "u",
                "generation,peer_id",
                "",
                "",
                "",
                "UNIQUE (generation, peer_id)",
            ),
            (
                "f",
                "generation",
                KEYPAIR_POOL_TABLE,
                "generation",
                "r/a/s",
                "",
            ),
            ("c", "slot_index", "", "", "", "CHECK ((slot_index >= 0))"),
        ]
    } else {
        return Err(LeaseAttachError::InvalidKeypairPoolShape);
    };
    actual.sort_unstable_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    expected.sort_unstable_by(|left, right| left.0.cmp(right.0).then_with(|| left.1.cmp(right.1)));
    let valid = actual.len() == expected.len()
        && actual
            .iter()
            .zip(expected.iter())
            .all(|(actual, expected)| {
                let (
                    kind,
                    columns,
                    referenced_table,
                    referenced_columns,
                    deferred,
                    initially_deferred,
                    validated,
                    supporting_index,
                    update_action,
                    delete_action,
                    match_type,
                    definition,
                    reference_schema_same,
                ) = actual;
                let (
                    expected_kind,
                    expected_columns,
                    expected_table,
                    expected_ref_columns,
                    expected_action,
                    expected_definition,
                ) = expected;
                let foreign_key_actions = *expected_kind != "f"
                    || format!("{delete_action}/{update_action}/{match_type}") == *expected_action;
                kind.as_str() == *expected_kind
                    && columns.as_str() == *expected_columns
                    && referenced_table.as_str() == *expected_table
                    && referenced_columns.as_str() == *expected_ref_columns
                    && *reference_schema_same
                    && !deferred
                    && !initially_deferred
                    && *validated
                    && (*expected_kind == "c" || *expected_kind == "f" || *supporting_index)
                    && foreign_key_actions
                    && (*expected_kind == "f" || definition.as_str() == *expected_definition)
            });
    if !valid {
        return Err(LeaseAttachError::InvalidKeypairPoolShape);
    }

    let expected_indexes = 2;
    let mut indexes = tx
        .query(
            "SELECT count(*)::bigint FROM pg_catalog.pg_index i \
             JOIN pg_catalog.pg_class c ON c.oid = i.indrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = ?",
            crate::db_params![schema, table],
        )
        .await?;
    if !matches!(indexes.next().await?, Some(row) if row.get::<i64>(0)? == expected_indexes) {
        return Err(LeaseAttachError::InvalidKeypairPoolShape);
    }
    Ok(())
}

async fn verify_keypool_acl(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), LeaseAttachError> {
    verify_read_only_relation_acl(tx, schema, KEYPAIR_POOL_TABLE).await?;
    verify_read_only_relation_acl(tx, schema, KEYPAIR_POOL_SLOTS_TABLE).await?;
    Ok(())
}

async fn verify_read_only_relation_acl(
    tx: &mut Transaction<'_>,
    schema: &str,
    table: &str,
) -> Result<(), LeaseAttachError> {
    let mut rows = tx
        .query(
            "SELECT relation.relowner = ns.nspowner \
                    AND relation.relowner <> runtime.oid \
                    AND NOT pg_catalog.pg_has_role(runtime.oid, relation.relowner, 'SET') \
                    AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'SELECT') \
                    AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, \
                      'INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER') \
                    AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, \
                      'SELECT WITH GRANT OPTION, INSERT WITH GRANT OPTION, \
                       UPDATE WITH GRANT OPTION, DELETE WITH GRANT OPTION, \
                       TRUNCATE WITH GRANT OPTION, REFERENCES WITH GRANT OPTION, \
                       TRIGGER WITH GRANT OPTION') \
                    AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000 \
                      THEN NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'MAINTAIN') \
                      ELSE true END \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                      COALESCE(relation.relacl, pg_catalog.acldefault('r', relation.relowner)) \
                    ) acl WHERE acl.grantee NOT IN (relation.relowner, runtime.oid) \
                      OR (acl.grantee = runtime.oid AND (acl.is_grantable \
                        OR acl.privilege_type <> 'SELECT'))) \
                    AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute a \
                      WHERE a.attrelid = relation.oid AND a.attnum > 0 \
                        AND NOT a.attisdropped AND a.attacl IS NOT NULL) \
             FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace ns ON ns.oid = relation.relnamespace \
             JOIN pg_catalog.pg_roles runtime ON runtime.rolname = current_user \
             WHERE ns.nspname = ? AND relation.relname = ?",
            crate::db_params![schema, table],
        )
        .await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<bool>(0)?) {
        return Err(LeaseAttachError::UnsafeRuntimeRole);
    }
    Ok(())
}

struct KeypairGuard {
    table: &'static str,
    trigger: &'static str,
    function: &'static str,
    trigger_type: i16,
    body: &'static str,
}

/// Row guards fire before INSERT, UPDATE and DELETE (the pool guard before
/// UPDATE and DELETE only); the truncate guards fire per statement.
const KEYPAIR_GUARDS: [KeypairGuard; 6] = [
    KeypairGuard {
        table: KEYPAIR_SLOT_TABLE,
        trigger: KEYPAIR_GUARD_TRIGGER,
        function: KEYPAIR_GUARD_FUNCTION,
        trigger_type: 31,
        body: KEYPAIR_GUARD_BODY,
    },
    KeypairGuard {
        table: KEYPAIR_POOL_TABLE,
        trigger: KEYPAIR_POOL_GUARD_TRIGGER,
        function: KEYPAIR_POOL_GUARD_FUNCTION,
        trigger_type: 27,
        body: KEYPAIR_POOL_GUARD_BODY,
    },
    KeypairGuard {
        table: KEYPAIR_POOL_SLOTS_TABLE,
        trigger: KEYPAIR_MAP_GUARD_TRIGGER,
        function: KEYPAIR_MAP_GUARD_FUNCTION,
        trigger_type: 31,
        body: KEYPAIR_MAP_GUARD_BODY,
    },
    KeypairGuard {
        table: KEYPAIR_SLOT_TABLE,
        trigger: KEYPAIR_SLOT_TRUNCATE_TRIGGER,
        function: KEYPAIR_TRUNCATE_GUARD_FUNCTION,
        trigger_type: 34,
        body: KEYPAIR_TRUNCATE_GUARD_BODY,
    },
    KeypairGuard {
        table: KEYPAIR_POOL_TABLE,
        trigger: KEYPAIR_POOL_TRUNCATE_TRIGGER,
        function: KEYPAIR_TRUNCATE_GUARD_FUNCTION,
        trigger_type: 34,
        body: KEYPAIR_TRUNCATE_GUARD_BODY,
    },
    KeypairGuard {
        table: KEYPAIR_POOL_SLOTS_TABLE,
        trigger: KEYPAIR_MAP_TRUNCATE_TRIGGER,
        function: KEYPAIR_TRUNCATE_GUARD_FUNCTION,
        trigger_type: 34,
        body: KEYPAIR_TRUNCATE_GUARD_BODY,
    },
];

async fn verify_keypair_guard(
    tx: &mut Transaction<'_>,
    schema: &str,
    runtime_role: Option<&str>,
) -> Result<(), LeaseAttachError> {
    for guard in &KEYPAIR_GUARDS {
        verify_one_keypair_guard(tx, schema, runtime_role, guard).await?;
    }
    Ok(())
}

/// One guard trigger and its function, pinned exactly. The functions run
/// with the privileges of the role whose statement fires them, so the slot
/// guard can read only what the runtime may read and no guard can write on
/// the owner's behalf. `runtime_role` names the role that must not hold
/// EXECUTE; `None` checks the session's own role.
async fn verify_one_keypair_guard(
    tx: &mut Transaction<'_>,
    schema: &str,
    runtime_role: Option<&str>,
    guard: &KeypairGuard,
) -> Result<(), LeaseAttachError> {
    let KeypairGuard {
        table,
        trigger: trigger_name,
        function: function_name,
        trigger_type,
        body: function_body,
    } = *guard;
    let function_path = format!("search_path=pg_catalog, {schema}, pg_temp");
    let mut rows = tx
        .query(
            "SELECT count(*)::bigint, \
                    COALESCE(bool_and(tr.tgname = ? AND NOT tr.tgisinternal \
                      AND tr.tgenabled = 'O' AND tr.tgtype = ? \
                      AND tr.tgnargs = 0 AND tr.tgqual IS NULL \
                      AND proc.proname = ? AND proc.proowner = ns.nspowner \
                      AND NOT proc.prosecdef AND proc.provolatile = 'v' \
                      AND proc.pronargs = 0 AND proc.prorettype = 'pg_catalog.trigger'::regtype \
                      AND language.lanname = 'plpgsql' \
                      AND proc.proconfig = ARRAY[?]::text[] \
                      AND pg_catalog.btrim(proc.prosrc) = pg_catalog.btrim(?) \
                      AND NOT pg_catalog.has_function_privilege( \
                        COALESCE(?, current_user::text)::name, proc.oid, 'EXECUTE') \
                      AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                        COALESCE(proc.proacl, pg_catalog.acldefault('f', proc.proowner)) \
                      ) acl WHERE acl.grantee <> proc.proowner)), false) \
             FROM pg_catalog.pg_trigger tr \
             JOIN pg_catalog.pg_class relation ON relation.oid = tr.tgrelid \
             JOIN pg_catalog.pg_namespace ns ON ns.oid = relation.relnamespace \
             JOIN pg_catalog.pg_proc proc ON proc.oid = tr.tgfoid \
             JOIN pg_catalog.pg_language language ON language.oid = proc.prolang \
             WHERE ns.nspname = ? AND relation.relname = ? \
               AND tr.tgname = ? AND NOT tr.tgisinternal",
            crate::db_params![
                trigger_name,
                trigger_type,
                function_name,
                function_path,
                function_body,
                runtime_role,
                schema,
                table,
                trigger_name,
            ],
        )
        .await?;
    if !matches!(rows.next().await?, Some(row) if row.get::<i64>(0)? == 1 && row.get::<bool>(1)?) {
        return Err(LeaseAttachError::InvalidSchemaShape);
    }
    Ok(())
}

async fn read_enrolled_peer_order(
    tx: &mut Transaction<'_>,
    schema: &str,
    expected_peer_ids: &[PeerId],
) -> Result<EnrolledKeypairPool, LeaseAttachError> {
    let quoted_schema = crate::db::physical_attestation::quote_identifier(schema);
    let mut rows = tx
        .query(
            &format!(
                "SELECT generation::text, state, activation_after <= now(), slot_count \
                 FROM {quoted_schema}.{KEYPAIR_POOL_TABLE} WHERE singleton"
            ),
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(LeaseAttachError::KeypoolGenerationPending);
    };
    let generation_text: String = row.get(0)?;
    let generation = uuid::Uuid::parse_str(&generation_text)
        .map_err(|_| LeaseAttachError::InvalidKeypairPoolShape)?;
    let state: String = row.get(1)?;
    let activation_due: bool = row.get(2)?;
    let slot_count: i32 = row.get(3)?;
    if state != "active" || !activation_due {
        return Err(LeaseAttachError::KeypoolGenerationPending);
    }
    if validate_peer_order(expected_peer_ids).is_err() {
        return Err(LeaseAttachError::KeypairPoolOrderMismatch);
    }
    let mut rows = tx
        .query(
            &format!(
                "SELECT slot_index, peer_id FROM {quoted_schema}.{KEYPAIR_POOL_SLOTS_TABLE} \
                 WHERE generation = ?::uuid ORDER BY slot_index"
            ),
            crate::db_params![generation.to_string()],
        )
        .await?;
    let mut peer_ids = Vec::new();
    while let Some(row) = rows.next().await? {
        let slot_index: i32 = row.get(0)?;
        let peer_id: String = row.get(1)?;
        if slot_index < 0 || slot_index as usize != peer_ids.len() {
            return Err(LeaseAttachError::InvalidKeypairPoolShape);
        }
        let parsed_peer_id =
            PeerId::from_str(&peer_id).map_err(|_| LeaseAttachError::InvalidKeypairPoolShape)?;
        if parsed_peer_id.to_string() != peer_id {
            return Err(LeaseAttachError::InvalidKeypairPoolShape);
        }
        peer_ids.push(parsed_peer_id);
    }
    if peer_ids.len() != slot_count as usize || peer_ids != expected_peer_ids {
        return Err(LeaseAttachError::KeypairPoolOrderMismatch);
    }
    Ok(EnrolledKeypairPool {
        generation,
        peer_ids,
    })
}

/// A cheap, stable, non-cryptographic offset from a node id, so pods spread
/// their initial slot scan instead of all starting at 0.
fn stable_offset(node_id: &str) -> usize {
    // FNV-1a over the bytes — deterministic and dependency-free.
    let mut hash: u64 = 0xcbf29ce484222325;
    for byte in node_id.as_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    hash as usize
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::{DatabaseConfig, DatabaseDriver};
    use std::time::Duration;

    // These tests share one `clustering_keypair_slots` table, so serialize
    // them and wipe the table at each start. They are skipped unless
    // `WADDLE_TEST_POSTGRES_URL` points at a Postgres (the control-plane CAS
    // has no SQLite equivalent). CI runs them under the clustering feature +
    // the Nix-spawned Postgres.
    fn ident() -> LeaseIdentity {
        LeaseIdentity {
            node_id: NodeId::generate(),
            node_epoch: uuid::Uuid::new_v4(),
        }
    }

    async fn clean_store() -> Option<PostgresKeypairSlotLease> {
        let url = waddle_ws_test_support::require_test_postgres_url()?;
        // `heartbeat` now renews through the control-plane pool, so the test
        // database needs one provisioned exactly like production's global
        // database does.
        let db = Database::from_config(
            "clustering-lease-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, url)
                .with_control_plane_pool(crate::db::DEFAULT_CONTROL_PLANE_POOL_SIZE),
        )
        .await
        .expect("open test postgres");
        let store = PostgresKeypairSlotLease::new(db);
        store.ensure_schema().await.expect("ensure schema");
        store
            .db
            .guard()
            .await
            .expect("guard")
            .execute("DELETE FROM clustering_keypair_slots", ())
            .await
            .expect("clean slots");
        Some(store)
    }

    const TTL: Duration = Duration::from_secs(30);

    #[tokio::test]
    async fn two_nodes_lease_distinct_slots() {
        let _guard = crate::clustering::keypair_slot_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        let slot_a = store.acquire(&a, 2, TTL).await.expect("a acquires");
        let slot_b = store.acquire(&b, 2, TTL).await.expect("b acquires");
        assert_ne!(slot_a.slot_index, slot_b.slot_index);
    }

    #[tokio::test]
    async fn single_slot_pool_admits_exactly_one() {
        let _guard = crate::clustering::keypair_slot_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        store.acquire(&a, 1, TTL).await.expect("a acquires slot 0");
        let err = store.acquire(&b, 1, TTL).await.expect_err("b is refused");
        assert!(matches!(err, LeaseError::NoFreeSlot { pool_size: 1 }));
    }

    #[tokio::test]
    async fn heartbeat_renews_and_release_frees() {
        let _guard = crate::clustering::keypair_slot_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        let slot = store.acquire(&a, 1, TTL).await.expect("a acquires");
        store.heartbeat(&a, slot, TTL).await.expect("a renews");
        // While A holds it, B cannot take it.
        assert!(store.acquire(&b, 1, TTL).await.is_err());
        // After A releases, B acquires immediately.
        store.release(&a, slot).await.expect("a releases");
        let slot_b = store.acquire(&b, 1, TTL).await.expect("b acquires freed");
        assert_eq!(slot_b.slot_index, slot.slot_index);
    }

    #[tokio::test]
    async fn expired_slot_is_stolen_and_old_holder_fences() {
        let _guard = crate::clustering::keypair_slot_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        let short_ttl = Duration::from_secs(1);
        let slot = store.acquire(&a, 1, short_ttl).await.expect("a acquires");
        // Let the lease lapse, then B steals the expired slot.
        tokio::time::sleep(Duration::from_millis(1_200)).await;
        let slot_b = store
            .acquire(&b, 1, short_ttl)
            .await
            .expect("b steals expired slot");
        assert_eq!(slot_b.slot_index, slot.slot_index);
        // A's renewal now fails — fencing loss.
        let err = store
            .heartbeat(&a, slot, short_ttl)
            .await
            .expect_err("a fences");
        assert!(matches!(err, LeaseError::FencingLoss { .. }));
    }

    #[tokio::test]
    async fn sub_second_ttl_uses_millisecond_precision() {
        let _guard = crate::clustering::keypair_slot_table_lock().lock().await;
        let Some(store) = clean_store().await else {
            return;
        };
        let (a, b) = (ident(), ident());
        // A 300ms TTL must NOT floor to '0 seconds' (which would make every
        // slot instantly stealable and every renewal fence).
        let ttl = Duration::from_millis(300);
        let slot = store.acquire(&a, 1, ttl).await.expect("a acquires");
        // Immediately renewable within the sub-second window, and the fresh
        // slot is not stealable by B.
        store
            .heartbeat(&a, slot, ttl)
            .await
            .expect("a renews in-window");
        assert!(store.acquire(&b, 1, ttl).await.is_err());
        // After the window lapses, B steals it.
        tokio::time::sleep(Duration::from_millis(450)).await;
        let slot_b = store.acquire(&b, 1, ttl).await.expect("b steals expired");
        assert_eq!(slot_b.slot_index, slot.slot_index);
    }

    #[test]
    fn stable_offset_is_deterministic() {
        assert_eq!(stable_offset("node-a"), stable_offset("node-a"));
    }
}
