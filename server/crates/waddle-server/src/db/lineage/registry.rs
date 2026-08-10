use std::{fmt, sync::Arc};

use async_trait::async_trait;
use futures::future::join_all;
use sqlx::{postgres::PgPool, sqlite::SqlitePool};

use crate::{
    config::LineageConfig,
    db::{Database, DatabaseDriver, DatabaseError},
};

use super::{
    verify, verify_via_control_plane, verify_via_pg_pool, verify_via_sqlite_pool, AttestedLineage,
    PgIdentity,
};

/// A durable storage boundary whose readiness is attested by this process.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DurableStore {
    Global,
    ControlPlane,
    Mam,
    Sm,
    PendingDelivery,
    Inbox,
    Pubsub,
    SpacesMetadata,
    ChannelSpaceLinks,
}

impl fmt::Display for DurableStore {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Global => "global",
            Self::ControlPlane => "control_plane",
            Self::Mam => "mam",
            Self::Sm => "sm",
            Self::PendingDelivery => "pending_delivery",
            Self::Inbox => "inbox",
            Self::Pubsub => "pubsub",
            Self::SpacesMetadata => "spaces_metadata",
            Self::ChannelSpaceLinks => "channel_space_links",
        })
    }
}

/// A typed, redaction-safe readiness reason.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageStatus {
    Initializing,
    MissingLineage,
    DeploymentUuidMismatch,
    SystemIdentifierUnavailable,
    /// Persisted PG identity (system identifier, database, or schema) does
    /// not match the live connection — the clone/restore/mis-point class.
    IdentityMismatch,
    /// The lineage table or row is structurally unusable (unknown format,
    /// invalid UUIDs, NULL identity fields, multiple rows, missing columns).
    MalformedLineage,
    /// A transport-level failure (connection loss, pool exhaustion) kept the
    /// probe from reaching the database at all. Transient by construction.
    ProbeError,
    ProbeTimeout,
    ClusteredSqlite,
    ClusteredEphemeral,
    ColocationMismatch,
    VerificationFailed,
}

impl LineageStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Initializing => "initializing",
            Self::MissingLineage => "missing_lineage",
            Self::DeploymentUuidMismatch => "deployment_uuid_mismatch",
            Self::SystemIdentifierUnavailable => "system_identifier_unavailable",
            Self::IdentityMismatch => "identity_mismatch",
            Self::MalformedLineage => "malformed_lineage",
            Self::ProbeError => "probe_error",
            Self::ProbeTimeout => "probe_timeout",
            Self::ClusteredSqlite => "clustered_sqlite",
            Self::ClusteredEphemeral => "clustered_ephemeral",
            Self::ColocationMismatch => "colocation_mismatch",
            Self::VerificationFailed => "verification_failed",
        }
    }
}

/// A pool-specific attestation result. The pool owns the physical connection
/// used for the query; registry callers never pass DSNs through readiness.
#[async_trait]
pub trait LineageAttestor: Send + Sync {
    async fn attest(&self, config: &LineageConfig) -> Result<AttestedLineage, DatabaseError>;
    fn driver(&self) -> DatabaseDriver;
}

#[derive(Clone)]
pub struct DatabaseLineageAttestor {
    database: Database,
}

#[derive(Clone)]
pub struct ControlPlaneLineageAttestor {
    database: Database,
}

#[derive(Clone)]
pub enum SqlxLineageBackend {
    Sqlite(SqlitePool),
    Postgres(PgPool),
}

#[derive(Clone)]
pub struct SqlxLineageAttestor {
    backend: SqlxLineageBackend,
}

impl ControlPlaneLineageAttestor {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl DatabaseLineageAttestor {
    pub fn new(database: Database) -> Self {
        Self { database }
    }
}

impl SqlxLineageAttestor {
    pub fn from_sqlite(pool: SqlitePool) -> Self {
        Self {
            backend: SqlxLineageBackend::Sqlite(pool),
        }
    }

    pub fn from_postgres(pool: PgPool) -> Self {
        Self {
            backend: SqlxLineageBackend::Postgres(pool),
        }
    }
}

#[async_trait]
impl LineageAttestor for DatabaseLineageAttestor {
    async fn attest(&self, config: &LineageConfig) -> Result<AttestedLineage, DatabaseError> {
        verify(&self.database, config).await
    }

    fn driver(&self) -> DatabaseDriver {
        self.database.driver()
    }
}

#[async_trait]
impl LineageAttestor for ControlPlaneLineageAttestor {
    async fn attest(&self, config: &LineageConfig) -> Result<AttestedLineage, DatabaseError> {
        verify_via_control_plane(&self.database, config).await
    }

    fn driver(&self) -> DatabaseDriver {
        self.database.driver()
    }
}

#[async_trait]
impl LineageAttestor for SqlxLineageAttestor {
    async fn attest(&self, config: &LineageConfig) -> Result<AttestedLineage, DatabaseError> {
        match &self.backend {
            SqlxLineageBackend::Sqlite(pool) => verify_via_sqlite_pool(pool, config).await,
            SqlxLineageBackend::Postgres(pool) => verify_via_pg_pool(pool, config).await,
        }
    }

    fn driver(&self) -> DatabaseDriver {
        match &self.backend {
            SqlxLineageBackend::Sqlite(_) => DatabaseDriver::Sqlite,
            SqlxLineageBackend::Postgres(_) => DatabaseDriver::Postgres,
        }
    }
}

/// Store topology: probes prove the pool they represent, aliases do not
/// duplicate shared pools, and ephemeral stores are exempt outside cluster mode.
pub enum LineageTopology {
    Probe {
        attestor: Arc<dyn LineageAttestor>,
        /// Set once on the first successful attestation. A pool's identity
        /// is immutable for the process lifetime (the DSN never changes and
        /// a proven boundary cannot silently become a different database
        /// through the same healthy pool), so a later TRANSPORT failure on a
        /// previously-proven store must not eject the node from the
        /// endpoint set. Definitive lineage errors always fail regardless.
        last_good: std::sync::OnceLock<AttestedLineage>,
    },
    Alias {
        of: DurableStore,
    },
    Ephemeral,
}

pub struct LineageRegistryEntry {
    pub store: DurableStore,
    pub topology: LineageTopology,
}

/// Immutable startup snapshot. It is assembled off-state and published once.
pub struct LineageRegistry {
    entries: Vec<LineageRegistryEntry>,
}

#[derive(Default)]
pub struct LineageRegistryBuilder {
    entries: Vec<LineageRegistryEntry>,
}

impl LineageRegistryBuilder {
    pub fn register_probe(&mut self, store: DurableStore, attestor: Arc<dyn LineageAttestor>) {
        self.entries.push(LineageRegistryEntry {
            store,
            topology: LineageTopology::Probe {
                attestor,
                last_good: std::sync::OnceLock::new(),
            },
        });
    }

    pub fn register_database(&mut self, store: DurableStore, database: Database) {
        self.register_probe(store, Arc::new(DatabaseLineageAttestor::new(database)));
    }

    pub fn register_control_plane(&mut self, store: DurableStore, database: Database) {
        self.register_probe(store, Arc::new(ControlPlaneLineageAttestor::new(database)));
    }

    pub fn register_alias(&mut self, store: DurableStore, of: DurableStore) {
        self.entries.push(LineageRegistryEntry {
            store,
            topology: LineageTopology::Alias { of },
        });
    }

    pub fn register_ephemeral(&mut self, store: DurableStore) {
        self.entries.push(LineageRegistryEntry {
            store,
            topology: LineageTopology::Ephemeral,
        });
    }

    pub fn seal(self) -> LineageRegistry {
        LineageRegistry::new(self.entries)
    }
}

impl LineageRegistry {
    pub fn new(entries: Vec<LineageRegistryEntry>) -> Self {
        Self { entries }
    }

    pub async fn attest(&self, config: &LineageConfig, clustering_enabled: bool) -> LineageReport {
        let mut failures = Vec::new();
        let probes = self
            .entries
            .iter()
            .filter_map(|entry| match &entry.topology {
                LineageTopology::Probe {
                    attestor,
                    last_good,
                } => Some((entry.store, Arc::clone(attestor), last_good)),
                LineageTopology::Alias { .. } => None,
                LineageTopology::Ephemeral => {
                    if clustering_enabled {
                        failures.push((entry.store, LineageStatus::ClusteredEphemeral));
                    }
                    None
                }
            });
        let results = join_all(probes.map(|(store, attestor, last_good)| async move {
            let driver = attestor.driver();
            (store, driver, last_good, attestor.attest(config).await)
        }))
        .await;
        let mut postgres = Vec::new();
        for (store, driver, last_good, result) in results {
            if clustering_enabled && driver == DatabaseDriver::Sqlite {
                failures.push((store, LineageStatus::ClusteredSqlite));
                continue;
            }
            match result {
                Ok(attested) => {
                    let _ = last_good.set(attested.clone());
                    if let Some(identity) = attested.postgres_identity.clone() {
                        postgres.push((store, attested, identity));
                    }
                }
                // Sticky success: ONLY a whitelisted transport-level error
                // (connection loss, pool exhaustion) on a boundary this
                // process already proved keeps the proven attestation — a
                // pool cannot silently become a different database while it
                // stays reachable. Every database-level error (any SQLSTATE:
                // dropped table, revoked grants, decode failures) and every
                // typed lineage error is definitive and always fails.
                Err(error) => match (is_transport_error(&error), last_good.get()) {
                    (false, _) | (_, None) => {
                        tracing::warn!(
                            store = %store,
                            error = %error,
                            "lineage attestation probe failed"
                        );
                        failures.push((store, status_for_error(&error)));
                    }
                    (true, Some(proven)) => {
                        tracing::warn!(
                            store = %store,
                            error = %error,
                            "lineage probe hit a transport error on a previously proven boundary; keeping the proven attestation"
                        );
                        if let Some(identity) = proven.postgres_identity.clone() {
                            postgres.push((store, proven.clone(), identity));
                        }
                    }
                },
            }
        }
        if clustering_enabled {
            mark_colocation_mismatches(&postgres, &mut failures);
        }
        LineageReport { failures }
    }
}

fn mark_colocation_mismatches(
    postgres: &[(DurableStore, AttestedLineage, PgIdentity)],
    failures: &mut Vec<(DurableStore, LineageStatus)>,
) {
    let Some((_, expected, expected_identity)) = postgres
        .iter()
        .find(|(store, _, _)| *store == DurableStore::Global)
    else {
        return;
    };
    for (store, actual, identity) in postgres {
        if *store == DurableStore::Global {
            continue;
        }
        if actual.lineage_uuid != expected.lineage_uuid || identity != expected_identity {
            failures.push((*store, LineageStatus::ColocationMismatch));
        }
    }
}

fn status_for_error(error: &DatabaseError) -> LineageStatus {
    match error {
        DatabaseError::Lineage(super::LineageError::MissingRow) => LineageStatus::MissingLineage,
        DatabaseError::Lineage(super::LineageError::DeploymentUuidMismatch { .. })
        | DatabaseError::Lineage(super::LineageError::DeploymentUuidUnconfigured { .. }) => {
            LineageStatus::DeploymentUuidMismatch
        }
        DatabaseError::Lineage(super::LineageError::SystemIdentifierUnavailable) => {
            LineageStatus::SystemIdentifierUnavailable
        }
        DatabaseError::Lineage(
            super::LineageError::SystemIdentifierMismatch { .. }
            | super::LineageError::DatabaseIdentityMismatch { .. }
            | super::LineageError::SchemaIdentityMismatch { .. },
        ) => LineageStatus::IdentityMismatch,
        DatabaseError::Lineage(
            super::LineageError::UnknownFormat { .. }
            | super::LineageError::MalformedTable { .. }
            | super::LineageError::InvalidUuid { .. }
            | super::LineageError::InvalidPostgresIdentity { .. },
        ) => LineageStatus::MalformedLineage,
        error if is_transport_error(error) => LineageStatus::ProbeError,
        _ => LineageStatus::VerificationFailed,
    }
}

/// Whitelist of errors that mean "the database was unreachable", as opposed
/// to "the database answered something disqualifying". Only these are
/// eligible for sticky-success and for the transient startup retry/self-heal
/// path; everything else is definitive.
fn is_transport_error(error: &DatabaseError) -> bool {
    match error {
        DatabaseError::ConnectionFailed(_) => true,
        DatabaseError::Internal(source) => matches!(
            source,
            sqlx::Error::Io(_)
                | sqlx::Error::Tls(_)
                | sqlx::Error::PoolTimedOut
                | sqlx::Error::PoolClosed
                | sqlx::Error::WorkerCrashed
        ),
        _ => false,
    }
}

#[derive(Debug, Clone, Default)]
pub struct LineageReport {
    failures: Vec<(DurableStore, LineageStatus)>,
}

impl LineageReport {
    pub fn is_attested(&self) -> bool {
        self.failures.is_empty()
    }

    pub fn failures(&self) -> &[(DurableStore, LineageStatus)] {
        &self.failures
    }

    pub fn timeout() -> Self {
        Self {
            failures: vec![(DurableStore::Global, LineageStatus::ProbeTimeout)],
        }
    }

    pub fn initializing() -> Self {
        Self {
            failures: vec![(DurableStore::Global, LineageStatus::Initializing)],
        }
    }

    /// True when every failure is transport-class (unreachable database /
    /// deadline), i.e. nothing definitive disqualified a boundary. The
    /// startup gate self-heals this class instead of latching.
    pub fn is_transient_only(&self) -> bool {
        !self.failures.is_empty()
            && self.failures.iter().all(|(_, status)| {
                matches!(
                    status,
                    LineageStatus::ProbeError | LineageStatus::ProbeTimeout
                )
            })
    }
}
