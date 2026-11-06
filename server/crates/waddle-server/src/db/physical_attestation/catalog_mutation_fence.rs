//! A dark, cooperative session advisory-lock experiment.
//!
//! Every reader and every privileged catalog mutator must use the same key on
//! the same PostgreSQL database. This module neither enforces that policy on
//! other sessions nor attests the primary, schema, roles, or pool hooks. In
//! particular, the shared guard is not authority for work on another session.
//! The caller must independently establish a direct, session-preserving PG17
//! primary. The dedicated connections are never returned to a pool.
//! Acquisition uses short autocommit `pg_try_advisory_lock` statements, with
//! no transaction or snapshot held while retrying. Blocking lock statements
//! can deadlock against REINDEX CONCURRENTLY's old-snapshot wait.
//! This requirement applies to every cooperating reader, including callers
//! using transaction-scoped locks: a failed try must end that transaction
//! before retrying. A session-fenced reader must end any earlier transaction
//! and snapshot before releasing its fence; a transaction-scoped fence must
//! last through commit or rollback. Polling is not fair and can starve a
//! waiting writer.
//!
//! A disconnected or cancelled session may remain active on the server. No
//! failure below proves its lock or an in-flight REINDEX has ended; an operator
//! must fence and reconcile the original backend before relying on its absence.
//! A cancelled acquisition cannot return a backend PID; callers need an
//! independent session identity and primary fence to reconcile it. Returned
//! PIDs are observations, not durable backend identities across restarts.
//! Connection and session setup each have a five-second deadline; the bounded
//! acquisition interval starts after setup and includes post-acquisition
//! verification. Individual operations are bounded, but lock ownership has no
//! TTL and is bounded only by explicit release or confirmed backend termination.
//!
//! The separate fenced-attestor observation uses the acquired reader's own
//! physical connection; a lock on some other connection cannot protect its
//! reads. It is a laboratory witness, not pool-hook integration or serving
//! authority. The current pinned hook rolls back and clears session advisory
//! locks, so passing a guard through that hook would not preserve this proof.
//! The lock is cooperative: unmodified old backends and any privileged DDL,
//! ACL, role, function, or index mutator that ignores it remain outside the
//! boundary. All must be independently excluded or fenced before use.

use std::time::Duration;

use sqlx::postgres::PgConnectOptions;
use sqlx::{Connection, PgConnection};
use thiserror::Error;

use super::{
    attest_dark_catalog_pinned, quote_identifier, validate_identifier,
    DarkPhysicalAttestationError, DarkPhysicalAttestationPins, PhysicalAttestationConfig,
};

const CONNECT_DEADLINE: Duration = Duration::from_secs(5);
const STATEMENT_DEADLINE: Duration = Duration::from_secs(25);
const MAX_LOCK_WAIT_MS: u128 = 15_000;
const LOCK_RETRY_INTERVAL: Duration = Duration::from_millis(20);

#[derive(Debug, Error)]
pub enum CatalogMutationFenceError {
    #[error("catalog fence key must be nonzero and lock wait must be 1 to 15000 ms")]
    InvalidBounds,
    #[error("catalog fence index names must be simple PostgreSQL identifiers")]
    InvalidIndex,
    #[error("catalog fence requires a writable PostgreSQL 17 server")]
    UnsupportedServer,
    #[error("catalog fence connection could not be established")]
    Connection,
    #[error("catalog fence remained contended through the bounded acquisition period")]
    Contended,
    #[error("catalog fence acquisition is unconfirmed; reconcile backend {backend_pid} (SQLSTATE {sqlstate:?})")]
    AcquisitionUnconfirmed {
        backend_pid: i32,
        sqlstate: Option<String>,
    },
    #[error("catalog fence guard is no longer usable")]
    FenceLost,
    #[error("catalog fence verification is unconfirmed; reconcile backend {backend_pid}")]
    FenceUnconfirmed { backend_pid: i32 },
    #[error("catalog maintenance outcome is unconfirmed; reconcile backend {backend_pid}")]
    MaintenanceUnconfirmed { backend_pid: i32 },
    #[error("catalog fence release is unconfirmed; reconcile backend {backend_pid}")]
    ReleaseUnconfirmed { backend_pid: i32 },
}

#[derive(Debug, Error)]
pub enum DarkFencedAttestorError {
    #[error(transparent)]
    Fence(#[from] CatalogMutationFenceError),
    #[error(transparent)]
    Attestation(#[from] DarkPhysicalAttestationError),
    #[error("fenced attestor transaction could not be started on backend {backend_pid}")]
    BeginUnconfirmed { backend_pid: i32 },
    #[error("fenced attestor rollback was not acknowledged on backend {backend_pid}")]
    RollbackUnconfirmed { backend_pid: i32 },
    #[error("fenced attestor observation timed out on backend {backend_pid}; reconcile it")]
    ObservationUnconfirmed { backend_pid: i32 },
    #[error("fenced attestor pause was cancelled")]
    PauseCancelled,
    #[error("fenced attestor PostgreSQL operation failed")]
    Postgres(#[from] sqlx::Error),
}

/// Test-only pause at an exact stage; the observer receives the fenced PID.
pub struct DarkFencedAttestorPause {
    pub ready: tokio::sync::oneshot::Sender<i32>,
    pub resume: tokio::sync::oneshot::Receiver<()>,
}

/// Pauses cannot run SQL or return the observed connection to the caller.
#[derive(Default)]
pub struct DarkFencedAttestorPauses {
    pub before_attestor: Option<DarkFencedAttestorPause>,
    pub after_attestor: Option<DarkFencedAttestorPause>,
    pub after_rollback: Option<DarkFencedAttestorPause>,
}

/// Observe the pinned attestor on the same dedicated PG17 session holding the
/// shared fence, from before its first protected read through acknowledged
/// rollback. No connection, transaction, or serving capability escapes.
///
/// Direct, session-preserving access to the authoritative primary and an
/// independently enforced all-mutator protocol are caller premises. This
/// cannot fence nonparticipants or older backends. The independent pins must
/// not be sourced from the session under inspection. An error or cancellation
/// can leave the original backend active: reconcile that backend on the fenced
/// primary before treating its transaction or lock as ended. In particular,
/// cancellation before acquisition returns no PID to its caller.
pub async fn inspect_dark_fenced_attestor(
    options: &PgConnectOptions,
    key: i64,
    wait: Duration,
    config: &PhysicalAttestationConfig,
    pins: DarkPhysicalAttestationPins,
    pauses: DarkFencedAttestorPauses,
) -> Result<(), DarkFencedAttestorError> {
    let session = acquire(options, key, wait, Mode::Shared).await?;
    let backend_pid = session.backend_pid;
    tokio::time::timeout(
        STATEMENT_DEADLINE,
        inspect_fenced_session(session, config, pins, pauses),
    )
    .await
    .map_err(|_| DarkFencedAttestorError::ObservationUnconfirmed { backend_pid })?
}

async fn inspect_fenced_session(
    mut session: HeldSession,
    config: &PhysicalAttestationConfig,
    pins: DarkPhysicalAttestationPins,
    pauses: DarkFencedAttestorPauses,
) -> Result<(), DarkFencedAttestorError> {
    let backend_pid = session.backend_pid;
    let mut tx = session
        .connection
        .begin_with("BEGIN ISOLATION LEVEL READ COMMITTED READ ONLY")
        .await
        .map_err(|_| DarkFencedAttestorError::BeginUnconfirmed { backend_pid })?;
    let observed = async {
        for statement in [
            "SET LOCAL lock_timeout = '5s'",
            "SET LOCAL statement_timeout = '10s'",
            "SET LOCAL idle_in_transaction_session_timeout = '10s'",
            "SET LOCAL transaction_timeout = '20s'",
        ] {
            sqlx::query(statement).execute(&mut *tx).await?;
        }
        verify_held_connection(&mut tx, backend_pid, session.key, session.mode).await?;
        pause(pauses.before_attestor, backend_pid).await?;
        let restore_path = format!(
            "{}, pg_temp",
            quote_identifier(&config.postgres().schema.name.0)
        );
        attest_dark_catalog_pinned(&mut tx, config, pins, &restore_path).await?;
        verify_held_connection(&mut tx, backend_pid, session.key, session.mode).await?;
        pause(pauses.after_attestor, backend_pid).await?;
        Ok::<(), DarkFencedAttestorError>(())
    }
    .await;
    tx.rollback()
        .await
        .map_err(|_| DarkFencedAttestorError::RollbackUnconfirmed { backend_pid })?;
    let after_rollback = if observed.is_ok() {
        pause(pauses.after_rollback, backend_pid).await
    } else {
        Ok(())
    };
    verify_held_connection(
        &mut session.connection,
        backend_pid,
        session.key,
        session.mode,
    )
    .await?;
    release(Some(session)).await?;
    observed?;
    after_rollback
}

async fn pause(
    pause: Option<DarkFencedAttestorPause>,
    backend_pid: i32,
) -> Result<(), DarkFencedAttestorError> {
    if let Some(pause) = pause {
        let _ = pause.ready.send(backend_pid);
        pause
            .resume
            .await
            .map_err(|_| DarkFencedAttestorError::PauseCancelled)?;
    }
    Ok(())
}

/// A qualified index name supplied by the trusted lab caller.
pub struct CatalogFenceIndex {
    schema: String,
    name: String,
}

impl CatalogFenceIndex {
    pub fn new(schema: &str, name: &str) -> Result<Self, CatalogMutationFenceError> {
        if validate_identifier(schema, "schema").is_err()
            || validate_identifier(name, "index").is_err()
        {
            return Err(CatalogMutationFenceError::InvalidIndex);
        }
        Ok(Self {
            schema: schema.to_owned(),
            name: name.to_owned(),
        })
    }
}

#[derive(Clone, Copy)]
enum Mode {
    Shared,
    Exclusive,
}

impl Mode {
    fn try_lock_sql(self) -> &'static str {
        match self {
            Self::Shared => "SELECT pg_catalog.pg_try_advisory_lock_shared($1)",
            Self::Exclusive => "SELECT pg_catalog.pg_try_advisory_lock($1)",
        }
    }

    fn unlock_sql(self) -> &'static str {
        match self {
            Self::Shared => "SELECT pg_catalog.pg_advisory_unlock_shared($1)",
            Self::Exclusive => "SELECT pg_catalog.pg_advisory_unlock($1)",
        }
    }

    fn lock_mode(self) -> &'static str {
        match self {
            Self::Shared => "ShareLock",
            Self::Exclusive => "ExclusiveLock",
        }
    }
}

struct HeldSession {
    connection: PgConnection,
    backend_pid: i32,
    key: i64,
    mode: Mode,
}

/// A non-cloneable session-held shared fence. It does not expose a connection.
pub struct CatalogFenceShared {
    session: Option<HeldSession>,
    backend_pid: i32,
}

/// A non-cloneable session-held exclusive fence. REINDEX uses this same session.
pub struct CatalogFenceExclusive {
    session: Option<HeldSession>,
    backend_pid: i32,
}

impl CatalogFenceShared {
    pub async fn acquire(
        options: &PgConnectOptions,
        key: i64,
        wait: Duration,
    ) -> Result<Self, CatalogMutationFenceError> {
        let session = acquire(options, key, wait, Mode::Shared).await?;
        Ok(Self {
            backend_pid: session.backend_pid,
            session: Some(session),
        })
    }

    pub fn backend_pid(&self) -> i32 {
        self.backend_pid
    }

    /// A failed or cancelled check invalidates this guard.
    pub async fn verify(&mut self) -> Result<(), CatalogMutationFenceError> {
        verify(&mut self.session).await
    }

    /// Success requires PostgreSQL to acknowledge that this session unlocked.
    pub async fn release(mut self) -> Result<(), CatalogMutationFenceError> {
        release(self.session.take()).await
    }
}

impl CatalogFenceExclusive {
    pub async fn acquire(
        options: &PgConnectOptions,
        key: i64,
        wait: Duration,
    ) -> Result<Self, CatalogMutationFenceError> {
        let session = acquire(options, key, wait, Mode::Exclusive).await?;
        Ok(Self {
            backend_pid: session.backend_pid,
            session: Some(session),
        })
    }

    pub fn backend_pid(&self) -> i32 {
        self.backend_pid
    }

    /// A failed or cancelled check invalidates this guard.
    pub async fn verify(&mut self) -> Result<(), CatalogMutationFenceError> {
        verify(&mut self.session).await
    }

    /// Execute only the fixed concurrent reindex operation on the locked session.
    /// An error or cancellation drops the dedicated session and invalidates the
    /// guard; it does not prove the server finished or rolled back maintenance.
    pub async fn reindex(
        &mut self,
        index: &CatalogFenceIndex,
    ) -> Result<(), CatalogMutationFenceError> {
        self.verify().await?;
        let mut session = self
            .session
            .take()
            .ok_or(CatalogMutationFenceError::FenceLost)?;
        let sql = format!(
            "REINDEX INDEX CONCURRENTLY {}.{}",
            quote_identifier(&index.schema),
            quote_identifier(&index.name)
        );
        let result = tokio::time::timeout(
            STATEMENT_DEADLINE,
            sqlx::query(&sql).execute(&mut session.connection),
        )
        .await;
        if !matches!(result, Ok(Ok(_)))
            || !matches!(session_is_held(&mut session).await, Some(true))
        {
            return Err(CatalogMutationFenceError::MaintenanceUnconfirmed {
                backend_pid: session.backend_pid,
            });
        }
        self.session = Some(session);
        Ok(())
    }

    pub async fn release(mut self) -> Result<(), CatalogMutationFenceError> {
        release(self.session.take()).await
    }
}

async fn acquire(
    options: &PgConnectOptions,
    key: i64,
    wait: Duration,
    mode: Mode,
) -> Result<HeldSession, CatalogMutationFenceError> {
    let wait_ms = wait.as_millis();
    if key == 0 || !(1..=MAX_LOCK_WAIT_MS).contains(&wait_ms) {
        return Err(CatalogMutationFenceError::InvalidBounds);
    }
    let mut connection =
        tokio::time::timeout(CONNECT_DEADLINE, PgConnection::connect_with(options))
            .await
            .map_err(|_| CatalogMutationFenceError::Connection)?
            .map_err(|_| CatalogMutationFenceError::Connection)?;
    let setup = async {
        sqlx::query("SET search_path = pg_catalog, pg_temp")
            .execute(&mut connection)
            .await?;
        sqlx::query("SET statement_timeout = '20s'")
            .execute(&mut connection)
            .await?;
        sqlx::query("SELECT pg_catalog.set_config('lock_timeout', $1, false)")
            .bind(format!("{wait_ms}ms"))
            .execute(&mut connection)
            .await?;
        sqlx::query_as::<_, (i32, bool, i32)>(
            "SELECT pg_catalog.current_setting('server_version_num')::pg_catalog.int4, \
             NOT pg_catalog.pg_is_in_recovery(), pg_catalog.pg_backend_pid()",
        )
        .fetch_one(&mut connection)
        .await
    };
    let (version, primary, backend_pid) = tokio::time::timeout(CONNECT_DEADLINE, setup)
        .await
        .map_err(|_| CatalogMutationFenceError::Connection)?
        .map_err(|_| CatalogMutationFenceError::Connection)?;
    if !(170_000..180_000).contains(&version) || !primary {
        return Err(CatalogMutationFenceError::UnsupportedServer);
    }
    let deadline = tokio::time::Instant::now() + wait;
    let mut observed_contention = false;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(if observed_contention {
                CatalogMutationFenceError::Contended
            } else {
                CatalogMutationFenceError::AcquisitionUnconfirmed {
                    backend_pid,
                    sqlstate: None,
                }
            });
        }
        let attempted = tokio::time::timeout(
            remaining,
            sqlx::query_scalar::<_, bool>(mode.try_lock_sql())
                .bind(key)
                .fetch_one(&mut connection),
        )
        .await;
        match attempted {
            Ok(Ok(true)) => break,
            Ok(Ok(false)) => {
                observed_contention = true;
                let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return Err(CatalogMutationFenceError::Contended);
                }
                tokio::time::sleep(LOCK_RETRY_INTERVAL.min(remaining)).await;
            }
            Ok(Err(error)) => {
                return Err(CatalogMutationFenceError::AcquisitionUnconfirmed {
                    backend_pid,
                    sqlstate: error
                        .as_database_error()
                        .and_then(|database| database.code().map(|code| code.into_owned())),
                });
            }
            Err(_) => {
                return Err(CatalogMutationFenceError::AcquisitionUnconfirmed {
                    backend_pid,
                    sqlstate: None,
                });
            }
        }
    }
    let mut session = HeldSession {
        connection,
        backend_pid,
        key,
        mode,
    };
    let verified = tokio::time::timeout_at(deadline, session_is_held(&mut session)).await;
    if !matches!(verified, Ok(Some(true))) {
        return Err(CatalogMutationFenceError::AcquisitionUnconfirmed {
            backend_pid,
            sqlstate: None,
        });
    }
    Ok(session)
}

async fn session_is_held(session: &mut HeldSession) -> Option<bool> {
    connection_holds_fence(
        &mut session.connection,
        session.backend_pid,
        session.key,
        session.mode,
    )
    .await
}

async fn verify_held_connection(
    connection: &mut PgConnection,
    backend_pid: i32,
    key: i64,
    mode: Mode,
) -> Result<(), CatalogMutationFenceError> {
    match connection_holds_fence(connection, backend_pid, key, mode).await {
        Some(true) => Ok(()),
        Some(false) => Err(CatalogMutationFenceError::FenceLost),
        None => Err(CatalogMutationFenceError::FenceUnconfirmed { backend_pid }),
    }
}

async fn connection_holds_fence(
    connection: &mut PgConnection,
    backend_pid: i32,
    key: i64,
    mode: Mode,
) -> Option<bool> {
    let observed = sqlx::query_scalar::<_, bool>(
        "SELECT pg_catalog.pg_backend_pid() = $1 AND NOT pg_catalog.pg_is_in_recovery() \
         AND EXISTS (SELECT 1 FROM pg_catalog.pg_locks \
           WHERE pid = pg_catalog.pg_backend_pid() AND locktype = 'advisory' \
             AND classid = (($2::pg_catalog.int8 >> 32) & 4294967295)::pg_catalog.oid \
             AND objid = ($2::pg_catalog.int8 & 4294967295)::pg_catalog.oid \
             AND objsubid = 1 AND mode = $3 AND granted)",
    )
    .bind(backend_pid)
    .bind(key)
    .bind(mode.lock_mode())
    .fetch_one(connection);
    tokio::time::timeout(STATEMENT_DEADLINE, observed)
        .await
        .ok()?
        .ok()
}

async fn verify(session: &mut Option<HeldSession>) -> Result<(), CatalogMutationFenceError> {
    let mut held = session.take().ok_or(CatalogMutationFenceError::FenceLost)?;
    match session_is_held(&mut held).await {
        Some(true) => {
            *session = Some(held);
            Ok(())
        }
        Some(false) => Err(CatalogMutationFenceError::FenceLost),
        None => Err(CatalogMutationFenceError::FenceUnconfirmed {
            backend_pid: held.backend_pid,
        }),
    }
}

async fn release(session: Option<HeldSession>) -> Result<(), CatalogMutationFenceError> {
    let mut held = session.ok_or(CatalogMutationFenceError::FenceLost)?;
    let unlocked = sqlx::query_scalar::<_, bool>(held.mode.unlock_sql())
        .bind(held.key)
        .fetch_one(&mut held.connection);
    match tokio::time::timeout(STATEMENT_DEADLINE, unlocked).await {
        Ok(Ok(true)) => Ok(()),
        Ok(Ok(false)) => Err(CatalogMutationFenceError::FenceLost),
        _ => Err(CatalogMutationFenceError::ReleaseUnconfirmed {
            backend_pid: held.backend_pid,
        }),
    }
}
