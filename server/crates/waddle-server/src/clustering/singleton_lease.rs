//! Unselected single-node lease candidate.
//!
//! The permanently provisioned slot serializes distinct process incarnations;
//! its lease row is separate so a writer holding the slot does not stop a
//! heartbeat. A slot guard holds its transaction until released. A fresh
//! lease permits admission, not a promise that the transaction finishes
//! before its deadline: a successor cannot replace the slot until the guard
//! has finished.
//!
//! This is not a serving authority. Existing claim and durable writers do not
//! use the slot, and this candidate does not attest its database, provision a
//! restricted role, fence old backends, or bound journal retention. Callers
//! must use a trusted owner-prepared schema and must not expose raw DML under
//! the eventual runtime role. Startup remains closed until those boundaries
//! are integrated and independently verified.
//!
//! Guard timeouts bound server-side transactions, not backend reconciliation.
//! A cancelled acquisition can leave a backend whose identity was never
//! returned to the caller. A returned guard identifies its backend and
//! transaction so an operator can reconcile an unconfirmed rollback on the
//! independently fenced primary. Prepared transactions are outside these
//! timeouts and must be disabled or separately fenced before selection.
//! These total and idle limits apply only to `guard`; register, renew, and
//! revoke retain per-lock and per-statement limits and still need their own
//! bounded-backend protocol before serving selection.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, Row};
use thiserror::Error;
use uuid::Uuid;

use crate::db::{Database, DatabaseDriver, DatabaseError, Transaction};

const MAX_LEASE_MILLIS: u128 = 3_600_000;
const GUARD_RELEASE_TIMEOUT: Duration = Duration::from_secs(1);

#[derive(Debug, Error)]
pub enum SingletonLeaseError {
    #[error("the candidate requires a PostgreSQL main-lane database")]
    PostgresRequired,
    #[error("owner preparation requires an unattested schema-owner connection")]
    OwnerRequired,
    #[error("invalid schema, node identity, operation identity, or lease duration")]
    InvalidInput,
    #[error("the provisioned singleton slot or its exact lease is missing or malformed")]
    InvalidAuthority,
    #[error("the operation ID is already bound to different input")]
    OperationMismatch,
    #[error("the singleton generation is exhausted")]
    GenerationExhausted,
    #[error("the slot is not owned by this exact live lease")]
    NotCurrent,
    #[error("the lease operation commit outcome is unknown for {operation_id}")]
    CommitUnknown { operation_id: Uuid },
    #[error("the singleton guard rollback is unconfirmed on {backend:?}")]
    GuardRollbackUnconfirmed { backend: GuardBackend },
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error(transparent)]
    Sql(#[from] sqlx::Error),
}

/// One configured node ID and a boot-unique, never-reused process UUID.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ProcessIdentity {
    node_id: String,
    incarnation: Uuid,
}

impl ProcessIdentity {
    pub fn new(node_id: impl Into<String>, incarnation: Uuid) -> Result<Self, SingletonLeaseError> {
        let node_id = node_id.into();
        if node_id.is_empty() || node_id.len() > 255 || incarnation.is_nil() {
            return Err(SingletonLeaseError::InvalidInput);
        }
        Ok(Self {
            node_id,
            incarnation,
        })
    }

    pub fn node_id(&self) -> &str {
        &self.node_id
    }

    pub fn incarnation(&self) -> Uuid {
        self.incarnation
    }
}

/// Exact identity and monotonic slot generation issued by a registration.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LeaseGrant {
    identity: ProcessIdentity,
    generation: i64,
}

/// Identity of a guard transaction on its original, independently fenced
/// PostgreSQL primary. PID alone is not enough because backends are reused.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GuardBackend {
    pub pid: i32,
    pub backend_start: DateTime<Utc>,
    pub transaction_start: DateTime<Utc>,
    pub postmaster_start: DateTime<Utc>,
    pub database_oid: u32,
    pub transaction_id: u64,
}

impl LeaseGrant {
    pub fn identity(&self) -> &ProcessIdentity {
        &self.identity
    }

    pub fn generation(&self) -> i64 {
        self.generation
    }
}

/// Complete immutable input for positive-only operation reconciliation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaseRequest {
    Register {
        operation_id: Uuid,
        identity: ProcessIdentity,
        ttl: Duration,
    },
    Renew {
        operation_id: Uuid,
        grant: LeaseGrant,
        ttl: Duration,
    },
    Revoke {
        operation_id: Uuid,
        grant: LeaseGrant,
    },
}

/// A recorded result is historical evidence, not proof a lease is still live.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaseOutcome {
    Acquired {
        grant: LeaseGrant,
        expires_at: DateTime<Utc>,
    },
    Busy,
    IncarnationReused,
    Renewed {
        grant: LeaseGrant,
        expires_at: DateTime<Utc>,
    },
    Lost,
    Revoked,
    NotCurrent,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum LeaseReadback {
    Recorded(LeaseOutcome),
    /// An absent receipt is not proof that the original backend rolled back.
    Unconfirmed,
}

struct Operation<'a> {
    id: Uuid,
    kind: &'static str,
    identity: &'a ProcessIdentity,
    generation: Option<i64>,
    ttl_ms: Option<i64>,
}

impl LeaseRequest {
    fn operation(&self) -> Result<Operation<'_>, SingletonLeaseError> {
        let (id, kind, identity, generation, ttl) = match self {
            Self::Register {
                operation_id,
                identity,
                ttl,
            } => (*operation_id, "register", identity, None, Some(*ttl)),
            Self::Renew {
                operation_id,
                grant,
                ttl,
            } => (
                *operation_id,
                "renew",
                &grant.identity,
                Some(grant.generation),
                Some(*ttl),
            ),
            Self::Revoke {
                operation_id,
                grant,
            } => (
                *operation_id,
                "revoke",
                &grant.identity,
                Some(grant.generation),
                None,
            ),
        };
        if id.is_nil() || generation.is_some_and(|value| value <= 0) {
            return Err(SingletonLeaseError::InvalidInput);
        }
        let ttl_ms = match ttl {
            Some(value)
                if value.as_millis() > 0
                    && value.as_millis() <= MAX_LEASE_MILLIS
                    && value.as_nanos().is_multiple_of(1_000_000) =>
            {
                Some(
                    i64::try_from(value.as_millis())
                        .map_err(|_| SingletonLeaseError::InvalidInput)?,
                )
            }
            Some(_) => return Err(SingletonLeaseError::InvalidInput),
            None => None,
        };
        Ok(Operation {
            id,
            kind,
            identity,
            generation,
            ttl_ms,
        })
    }
}

/// Install a fresh candidate schema as its existing owner. This deliberately
/// refuses pre-existing relations rather than repairing or resetting them.
pub async fn prepare_owner_schema(db: &Database, schema: &str) -> Result<(), SingletonLeaseError> {
    validate_db(db)?;
    if db.uses_attested_postgres_pool() {
        return Err(SingletonLeaseError::OwnerRequired);
    }
    let schema = quoted_schema(schema)?;
    let mut tx = bounded_transaction(db).await?;
    let owner: bool = sqlx::query_scalar(
        "SELECT n.nspowner = (SELECT oid FROM pg_catalog.pg_roles WHERE rolname = current_user) \
         FROM pg_catalog.pg_namespace n WHERE n.nspname = $1",
    )
    .bind(schema.trim_matches('"'))
    .fetch_optional(pg(&mut tx)?)
    .await?
    .unwrap_or(false);
    if !owner {
        return Err(SingletonLeaseError::OwnerRequired);
    }
    tx.execute_batch(&format!(
        r#"
        CREATE TABLE {schema}.singleton_candidate_leases (
            incarnation UUID PRIMARY KEY,
            node_id TEXT NOT NULL CHECK (octet_length(node_id) BETWEEN 1 AND 255),
            generation BIGINT NOT NULL UNIQUE CHECK (generation > 0),
            expires_at TIMESTAMPTZ NOT NULL,
            revoked BOOLEAN NOT NULL DEFAULT FALSE,
            UNIQUE (node_id, incarnation, generation)
        );
        CREATE TABLE {schema}.singleton_candidate_slot (
            slot SMALLINT PRIMARY KEY CHECK (slot = 1),
            node_id TEXT,
            incarnation UUID,
            generation BIGINT NOT NULL,
            CHECK ((node_id IS NULL AND incarnation IS NULL AND generation = 0)
                OR (node_id IS NOT NULL AND incarnation IS NOT NULL AND generation > 0)),
            FOREIGN KEY (node_id, incarnation, generation)
                REFERENCES {schema}.singleton_candidate_leases (node_id, incarnation, generation)
        );
        INSERT INTO {schema}.singleton_candidate_slot (slot, generation) VALUES (1, 0);
        CREATE TABLE {schema}.singleton_candidate_operations (
            operation_id UUID PRIMARY KEY,
            kind TEXT NOT NULL CHECK (kind IN ('register', 'renew', 'revoke')),
            node_id TEXT NOT NULL,
            incarnation UUID NOT NULL,
            expected_generation BIGINT,
            ttl_ms BIGINT,
            result_kind TEXT,
            result_generation BIGINT,
            result_expires_at TIMESTAMPTZ
        );
        REVOKE ALL ON {schema}.singleton_candidate_leases FROM PUBLIC;
        REVOKE ALL ON {schema}.singleton_candidate_slot FROM PUBLIC;
        REVOKE ALL ON {schema}.singleton_candidate_operations FROM PUBLIC;
        "#
    ))
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Closed, unselected API over a caller-supplied PostgreSQL main pool.
/// Creating it does not attest the database or confer serving authority.
#[derive(Clone)]
pub struct SingletonLeaseStore {
    db: Database,
    schema: String,
}

impl SingletonLeaseStore {
    pub fn new_unverified(db: Database, schema: &str) -> Result<Self, SingletonLeaseError> {
        validate_db(&db)?;
        Ok(Self {
            db,
            schema: quoted_schema(schema)?,
        })
    }

    pub async fn register(
        &self,
        operation_id: Uuid,
        identity: ProcessIdentity,
        ttl: Duration,
    ) -> Result<LeaseOutcome, SingletonLeaseError> {
        self.execute(LeaseRequest::Register {
            operation_id,
            identity,
            ttl,
        })
        .await
    }

    pub async fn renew(
        &self,
        operation_id: Uuid,
        grant: LeaseGrant,
        ttl: Duration,
    ) -> Result<LeaseOutcome, SingletonLeaseError> {
        self.execute(LeaseRequest::Renew {
            operation_id,
            grant,
            ttl,
        })
        .await
    }

    pub async fn revoke(
        &self,
        operation_id: Uuid,
        grant: LeaseGrant,
    ) -> Result<LeaseOutcome, SingletonLeaseError> {
        self.execute(LeaseRequest::Revoke {
            operation_id,
            grant,
        })
        .await
    }

    /// A missing record is deliberately inconclusive. Retain the original
    /// operation identity and reconcile its backend/primary before concluding
    /// that a previously uncertain commit did not happen.
    pub async fn readback(
        &self,
        request: &LeaseRequest,
    ) -> Result<LeaseReadback, SingletonLeaseError> {
        let operation = request.operation()?;
        let mut tx = bounded_transaction(&self.db).await?;
        let result = read_operation(pg(&mut tx)?, &self.schema, &operation).await?;
        tx.rollback().await?;
        Ok(match result {
            Some(outcome) => LeaseReadback::Recorded(outcome),
            None => LeaseReadback::Unconfirmed,
        })
    }

    async fn execute(&self, request: LeaseRequest) -> Result<LeaseOutcome, SingletonLeaseError> {
        let operation = request.operation()?;
        let mut tx = bounded_transaction(&self.db).await?;
        let inserted = sqlx::query(&format!(
            "INSERT INTO {}.singleton_candidate_operations \
             (operation_id, kind, node_id, incarnation, expected_generation, ttl_ms) \
             VALUES ($1, $2, $3, $4, $5, $6) ON CONFLICT (operation_id) DO NOTHING",
            self.schema
        ))
        .bind(operation.id)
        .bind(operation.kind)
        .bind(&operation.identity.node_id)
        .bind(operation.identity.incarnation)
        .bind(operation.generation)
        .bind(operation.ttl_ms)
        .execute(pg(&mut tx)?)
        .await?
        .rows_affected();
        if inserted == 0 {
            let outcome = read_operation(pg(&mut tx)?, &self.schema, &operation)
                .await?
                .ok_or(SingletonLeaseError::InvalidAuthority)?;
            tx.rollback().await?;
            return Ok(outcome);
        }
        let outcome = match &request {
            LeaseRequest::Register { .. } => self.register_inner(&mut tx, &operation).await?,
            LeaseRequest::Renew { .. } => self.renew_inner(&mut tx, &operation).await?,
            LeaseRequest::Revoke { .. } => self.revoke_inner(&mut tx, &operation).await?,
        };
        write_operation(pg(&mut tx)?, &self.schema, operation.id, &outcome).await?;
        if tx.commit().await.is_err() {
            return Err(SingletonLeaseError::CommitUnknown {
                operation_id: operation.id,
            });
        }
        Ok(outcome)
    }

    async fn register_inner(
        &self,
        tx: &mut Transaction<'_>,
        op: &Operation<'_>,
    ) -> Result<LeaseOutcome, SingletonLeaseError> {
        lock_slot(pg(tx)?, &self.schema, false).await?;
        let current = read_slot(pg(tx)?, &self.schema).await?;
        if let Some((node_id, incarnation)) = &current.owner {
            lock_lease(pg(tx)?, &self.schema, *incarnation).await?;
            let lease = read_lease(pg(tx)?, &self.schema, *incarnation).await?;
            if lease.node_id != *node_id || lease.generation != current.generation {
                return Err(SingletonLeaseError::InvalidAuthority);
            }
            if !lease.revoked && lease.fresh {
                return Ok(LeaseOutcome::Busy);
            }
        }
        let reused: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM {}.singleton_candidate_leases WHERE incarnation = $1)",
            self.schema
        ))
        .bind(op.identity.incarnation)
        .fetch_one(pg(tx)?)
        .await?;
        if reused {
            return Ok(LeaseOutcome::IncarnationReused);
        }
        let generation = current
            .generation
            .checked_add(1)
            .ok_or(SingletonLeaseError::GenerationExhausted)?;
        if let Some((_, incarnation)) = current.owner {
            sqlx::query(&format!(
                "UPDATE {}.singleton_candidate_leases SET revoked = TRUE WHERE incarnation = $1",
                self.schema
            ))
            .bind(incarnation)
            .execute(pg(tx)?)
            .await?;
        }
        let expires_at: DateTime<Utc> = sqlx::query_scalar(&format!(
            "INSERT INTO {}.singleton_candidate_leases (node_id, incarnation, generation, expires_at) \
             VALUES ($1, $2, $3, pg_catalog.clock_timestamp() + ($4::double precision * interval '1 millisecond')) \
             RETURNING expires_at",
            self.schema
        ))
        .bind(&op.identity.node_id)
        .bind(op.identity.incarnation)
        .bind(generation)
        .bind(op.ttl_ms.ok_or(SingletonLeaseError::InvalidInput)? as f64)
        .fetch_one(pg(tx)?)
        .await?;
        sqlx::query(&format!(
            "UPDATE {}.singleton_candidate_slot SET node_id = $1, incarnation = $2, generation = $3 WHERE slot = 1",
            self.schema
        ))
        .bind(&op.identity.node_id)
        .bind(op.identity.incarnation)
        .bind(generation)
        .execute(pg(tx)?)
        .await?;
        Ok(LeaseOutcome::Acquired {
            grant: LeaseGrant {
                identity: op.identity.clone(),
                generation,
            },
            expires_at,
        })
    }

    async fn renew_inner(
        &self,
        tx: &mut Transaction<'_>,
        op: &Operation<'_>,
    ) -> Result<LeaseOutcome, SingletonLeaseError> {
        // No slot lock: a heartbeat must progress while a writer holds it.
        if !try_lock_lease(pg(tx)?, &self.schema, op.identity.incarnation).await? {
            return Ok(LeaseOutcome::Lost);
        }
        let lease = read_lease(pg(tx)?, &self.schema, op.identity.incarnation).await?;
        let generation = op.generation.ok_or(SingletonLeaseError::InvalidInput)?;
        if lease.node_id != op.identity.node_id
            || lease.generation != generation
            || lease.revoked
            || !lease.fresh
        {
            return Ok(LeaseOutcome::Lost);
        }
        // This is an unlocked MVCC read, not a slot row lock. It detects a
        // missing or mismatched slot without making heartbeats wait on an
        // ordinary slot guard. Replacement also locks this lease row and
        // irrevocably revokes it in the same transaction as the slot change.
        let current: bool = sqlx::query_scalar(&format!(
            "SELECT EXISTS (SELECT 1 FROM {}.singleton_candidate_slot \
             WHERE slot = 1 AND node_id = $1 AND incarnation = $2 AND generation = $3)",
            self.schema
        ))
        .bind(&op.identity.node_id)
        .bind(op.identity.incarnation)
        .bind(generation)
        .fetch_one(pg(tx)?)
        .await?;
        if !current {
            return Ok(LeaseOutcome::Lost);
        }
        let expires_at: Option<DateTime<Utc>> = sqlx::query_scalar(&format!(
            "UPDATE {}.singleton_candidate_leases SET expires_at = GREATEST(expires_at, \
             pg_catalog.clock_timestamp() + ($1::double precision * interval '1 millisecond')) \
             WHERE incarnation = $2 AND node_id = $3 AND generation = $4 AND NOT revoked \
             AND expires_at > pg_catalog.clock_timestamp() RETURNING expires_at",
            self.schema
        ))
        .bind(op.ttl_ms.ok_or(SingletonLeaseError::InvalidInput)? as f64)
        .bind(op.identity.incarnation)
        .bind(&op.identity.node_id)
        .bind(generation)
        .fetch_optional(pg(tx)?)
        .await?;
        Ok(match expires_at {
            Some(expires_at) => LeaseOutcome::Renewed {
                grant: LeaseGrant {
                    identity: op.identity.clone(),
                    generation,
                },
                expires_at,
            },
            None => LeaseOutcome::Lost,
        })
    }

    async fn revoke_inner(
        &self,
        tx: &mut Transaction<'_>,
        op: &Operation<'_>,
    ) -> Result<LeaseOutcome, SingletonLeaseError> {
        lock_slot(pg(tx)?, &self.schema, false).await?;
        let slot = read_slot(pg(tx)?, &self.schema).await?;
        let generation = op.generation.ok_or(SingletonLeaseError::InvalidInput)?;
        if slot.owner.as_ref() != Some(&(op.identity.node_id.clone(), op.identity.incarnation))
            || slot.generation != generation
        {
            return Ok(LeaseOutcome::NotCurrent);
        }
        lock_lease(pg(tx)?, &self.schema, op.identity.incarnation).await?;
        let lease = read_lease(pg(tx)?, &self.schema, op.identity.incarnation).await?;
        if lease.node_id != op.identity.node_id || lease.generation != generation {
            return Err(SingletonLeaseError::InvalidAuthority);
        }
        if lease.revoked {
            return Ok(LeaseOutcome::NotCurrent);
        }
        sqlx::query(&format!(
            "UPDATE {}.singleton_candidate_leases SET revoked = TRUE WHERE incarnation = $1",
            self.schema
        ))
        .bind(op.identity.incarnation)
        .execute(pg(tx)?)
        .await?;
        Ok(LeaseOutcome::Revoked)
    }

    /// Hold the exact slot against replacement with bounded server-side idle
    /// and total transaction time. The guard exposes no raw SQL or transaction
    /// and does not authorize a write on another connection. Keeping the Rust
    /// value alive does not prove the server still holds the lock: a timeout
    /// can terminate its backend. If acquisition is cancelled, it has no
    /// result or backend identity; dropping its transaction requests rollback
    /// but does not prove the server has released its locks.
    pub async fn guard<'a>(
        &'a self,
        grant: &LeaseGrant,
    ) -> Result<SlotGuard<'a>, SingletonLeaseError> {
        let mut tx = bounded_transaction(&self.db).await?;
        // Install these before the first slot lock. PostgreSQL 17's
        // transaction timeout bounds an active transaction as well as an idle
        // one; lock_timeout and statement_timeout alone do neither.
        tx.execute("SET LOCAL idle_in_transaction_session_timeout = '2s'", ())
            .await?;
        tx.execute("SET LOCAL transaction_timeout = '5s'", ())
            .await?;
        let backend = guard_backend(pg(&mut tx)?).await?;
        let admitted = async {
            lock_slot(pg(&mut tx)?, &self.schema, true).await?;
            let slot = read_slot(pg(&mut tx)?, &self.schema).await?;
            if slot.owner.as_ref()
                != Some(&(grant.identity.node_id.clone(), grant.identity.incarnation))
                || slot.generation != grant.generation
            {
                return Err(SingletonLeaseError::NotCurrent);
            }
            let lease = read_lease(pg(&mut tx)?, &self.schema, grant.identity.incarnation).await?;
            if lease.node_id != grant.identity.node_id
                || lease.generation != grant.generation
                || lease.revoked
                || !lease.fresh
            {
                return Err(SingletonLeaseError::NotCurrent);
            }
            Ok(())
        }
        .await;
        match admitted {
            Ok(()) => Ok(SlotGuard { tx, backend }),
            Err(error) => {
                rollback_guard(tx, backend).await?;
                Err(error)
            }
        }
    }
}

pub struct SlotGuard<'a> {
    tx: Transaction<'a>,
    backend: GuardBackend,
}

impl SlotGuard<'_> {
    /// Save this before a cancellable wait or release so an unconfirmed
    /// rollback can be reconciled on the same fenced primary.
    pub fn backend(&self) -> GuardBackend {
        self.backend
    }

    /// Keep a guard active in the PG17 transaction-deadline test.
    #[cfg(feature = "test-support")]
    pub async fn pulse_for_test(&mut self) -> Result<(), SingletonLeaseError> {
        sqlx::query("SELECT 1").execute(pg(&mut self.tx)?).await?;
        Ok(())
    }

    /// A successful return confirms rollback. An error identifies the
    /// original backend and transaction but does not claim either has ended.
    pub async fn release(self) -> Result<(), SingletonLeaseError> {
        rollback_guard(self.tx, self.backend).await
    }
}

async fn rollback_guard(
    tx: Transaction<'_>,
    backend: GuardBackend,
) -> Result<(), SingletonLeaseError> {
    match tokio::time::timeout(GUARD_RELEASE_TIMEOUT, tx.rollback()).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) | Err(_) => Err(SingletonLeaseError::GuardRollbackUnconfirmed { backend }),
    }
}

async fn guard_backend(conn: &mut PgConnection) -> Result<GuardBackend, SingletonLeaseError> {
    // Assign an xid before acquiring the slot. Backend identity plus xid lets
    // a trusted observer distinguish this transaction from later pool reuse.
    let row = sqlx::query(
        "SELECT activity.pid, activity.backend_start, activity.xact_start, \
                activity.datid::bigint, pg_catalog.pg_postmaster_start_time(), \
                pg_catalog.pg_current_xact_id()::text \
         FROM pg_catalog.pg_stat_activity AS activity \
         WHERE activity.pid = pg_catalog.pg_backend_pid()",
    )
    .fetch_optional(conn)
    .await?
    .ok_or(SingletonLeaseError::InvalidAuthority)?;
    let database_oid = u32::try_from(row.try_get::<i64, _>(3)?)
        .map_err(|_| SingletonLeaseError::InvalidAuthority)?;
    let transaction_id = row
        .try_get::<String, _>(5)?
        .parse()
        .map_err(|_| SingletonLeaseError::InvalidAuthority)?;
    Ok(GuardBackend {
        pid: row.try_get(0)?,
        backend_start: row.try_get(1)?,
        transaction_start: row.try_get(2)?,
        postmaster_start: row.try_get(4)?,
        database_oid,
        transaction_id,
    })
}

fn validate_db(db: &Database) -> Result<(), SingletonLeaseError> {
    if db.driver() != DatabaseDriver::Postgres || !db.is_main_lane() {
        return Err(SingletonLeaseError::PostgresRequired);
    }
    Ok(())
}

fn quoted_schema(schema: &str) -> Result<String, SingletonLeaseError> {
    crate::db::physical_attestation::validate_identifier(schema, "singleton schema")
        .map_err(|_| SingletonLeaseError::InvalidInput)?;
    Ok(crate::db::physical_attestation::quote_identifier(schema))
}

async fn bounded_transaction(db: &Database) -> Result<Transaction<'_>, SingletonLeaseError> {
    let mut tx = db.begin().await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE",
        (),
    )
    .await?;
    tx.execute("SET LOCAL search_path = pg_catalog", ()).await?;
    tx.execute("SET LOCAL lock_timeout = '3s'", ()).await?;
    tx.execute("SET LOCAL statement_timeout = '5s'", ()).await?;
    Ok(tx)
}

fn pg<'a>(tx: &'a mut Transaction<'_>) -> Result<&'a mut PgConnection, SingletonLeaseError> {
    tx.postgres_connection()
        .ok_or(SingletonLeaseError::PostgresRequired)
}

async fn lock_slot(
    conn: &mut PgConnection,
    schema: &str,
    shared: bool,
) -> Result<(), SingletonLeaseError> {
    let lock = if shared {
        "FOR SHARE NOWAIT"
    } else {
        "FOR UPDATE"
    };
    let found: Option<i16> = sqlx::query_scalar(&format!(
        "SELECT slot FROM {schema}.singleton_candidate_slot WHERE slot = 1 {lock}"
    ))
    .fetch_optional(conn)
    .await?;
    found
        .map(|_| ())
        .ok_or(SingletonLeaseError::InvalidAuthority)
}

struct Slot {
    owner: Option<(String, Uuid)>,
    generation: i64,
}

async fn read_slot(conn: &mut PgConnection, schema: &str) -> Result<Slot, SingletonLeaseError> {
    let row = sqlx::query(&format!(
        "SELECT node_id, incarnation, generation FROM {schema}.singleton_candidate_slot WHERE slot = 1"
    ))
    .fetch_optional(conn)
    .await?
    .ok_or(SingletonLeaseError::InvalidAuthority)?;
    let node_id: Option<String> = row.try_get(0)?;
    let incarnation: Option<Uuid> = row.try_get(1)?;
    let generation: i64 = row.try_get(2)?;
    let owner = match (node_id, incarnation, generation) {
        (None, None, 0) => None,
        (Some(node_id), Some(incarnation), value) if value > 0 => Some((node_id, incarnation)),
        _ => return Err(SingletonLeaseError::InvalidAuthority),
    };
    Ok(Slot { owner, generation })
}

async fn try_lock_lease(
    conn: &mut PgConnection,
    schema: &str,
    incarnation: Uuid,
) -> Result<bool, SingletonLeaseError> {
    let found: Option<Uuid> = sqlx::query_scalar(&format!(
        "SELECT incarnation FROM {schema}.singleton_candidate_leases WHERE incarnation = $1 FOR UPDATE"
    ))
    .bind(incarnation)
    .fetch_optional(conn)
    .await?;
    Ok(found.is_some())
}

async fn lock_lease(
    conn: &mut PgConnection,
    schema: &str,
    incarnation: Uuid,
) -> Result<(), SingletonLeaseError> {
    if !try_lock_lease(conn, schema, incarnation).await? {
        return Err(SingletonLeaseError::InvalidAuthority);
    }
    Ok(())
}

struct LeaseRow {
    node_id: String,
    generation: i64,
    revoked: bool,
    fresh: bool,
}

async fn read_lease(
    conn: &mut PgConnection,
    schema: &str,
    incarnation: Uuid,
) -> Result<LeaseRow, SingletonLeaseError> {
    let row = sqlx::query(&format!(
        "SELECT node_id, generation, revoked, expires_at > pg_catalog.clock_timestamp() \
         FROM {schema}.singleton_candidate_leases WHERE incarnation = $1"
    ))
    .bind(incarnation)
    .fetch_optional(conn)
    .await?
    .ok_or(SingletonLeaseError::InvalidAuthority)?;
    Ok(LeaseRow {
        node_id: row.try_get(0)?,
        generation: row.try_get(1)?,
        revoked: row.try_get(2)?,
        fresh: row.try_get(3)?,
    })
}

async fn read_operation(
    conn: &mut PgConnection,
    schema: &str,
    op: &Operation<'_>,
) -> Result<Option<LeaseOutcome>, SingletonLeaseError> {
    let row = sqlx::query(&format!(
        "SELECT kind, node_id, incarnation, expected_generation, ttl_ms, result_kind, \
         result_generation, result_expires_at FROM {schema}.singleton_candidate_operations WHERE operation_id = $1"
    ))
    .bind(op.id)
    .fetch_optional(conn)
    .await?;
    let Some(row) = row else { return Ok(None) };
    if row.try_get::<String, _>(0)? != op.kind
        || row.try_get::<String, _>(1)? != op.identity.node_id
        || row.try_get::<Uuid, _>(2)? != op.identity.incarnation
        || row.try_get::<Option<i64>, _>(3)? != op.generation
        || row.try_get::<Option<i64>, _>(4)? != op.ttl_ms
    {
        return Err(SingletonLeaseError::OperationMismatch);
    }
    let kind: Option<String> = row.try_get(5)?;
    let generation: Option<i64> = row.try_get(6)?;
    let expires_at: Option<DateTime<Utc>> = row.try_get(7)?;
    let outcome = match (op.kind, kind.as_deref(), generation, expires_at) {
        ("register", Some("acquired"), Some(generation), Some(expires_at)) if generation > 0 => {
            LeaseOutcome::Acquired {
                grant: LeaseGrant {
                    identity: op.identity.clone(),
                    generation,
                },
                expires_at,
            }
        }
        ("register", Some("busy"), None, None) => LeaseOutcome::Busy,
        ("register", Some("reused"), None, None) => LeaseOutcome::IncarnationReused,
        ("renew", Some("renewed"), Some(generation), Some(expires_at))
            if Some(generation) == op.generation =>
        {
            LeaseOutcome::Renewed {
                grant: LeaseGrant {
                    identity: op.identity.clone(),
                    generation,
                },
                expires_at,
            }
        }
        ("renew", Some("lost"), None, None) => LeaseOutcome::Lost,
        ("revoke", Some("revoked"), None, None) => LeaseOutcome::Revoked,
        ("revoke", Some("not_current"), None, None) => LeaseOutcome::NotCurrent,
        _ => return Err(SingletonLeaseError::InvalidAuthority),
    };
    Ok(Some(outcome))
}

async fn write_operation(
    conn: &mut PgConnection,
    schema: &str,
    id: Uuid,
    outcome: &LeaseOutcome,
) -> Result<(), SingletonLeaseError> {
    let (kind, generation, expires_at) = match outcome {
        LeaseOutcome::Acquired { grant, expires_at } => {
            ("acquired", Some(grant.generation), Some(*expires_at))
        }
        LeaseOutcome::Busy => ("busy", None, None),
        LeaseOutcome::IncarnationReused => ("reused", None, None),
        LeaseOutcome::Renewed { grant, expires_at } => {
            ("renewed", Some(grant.generation), Some(*expires_at))
        }
        LeaseOutcome::Lost => ("lost", None, None),
        LeaseOutcome::Revoked => ("revoked", None, None),
        LeaseOutcome::NotCurrent => ("not_current", None, None),
    };
    let changed = sqlx::query(&format!(
        "UPDATE {schema}.singleton_candidate_operations SET result_kind = $1, \
         result_generation = $2, result_expires_at = $3 WHERE operation_id = $4 AND result_kind IS NULL"
    ))
    .bind(kind)
    .bind(generation)
    .bind(expires_at)
    .bind(id)
    .execute(conn)
    .await?
    .rows_affected();
    if changed != 1 {
        return Err(SingletonLeaseError::InvalidAuthority);
    }
    Ok(())
}
