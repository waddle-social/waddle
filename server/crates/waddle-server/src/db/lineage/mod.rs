//! Durable database lineage attestation.
//!
//! `_lineage` is intentionally bootstrap-managed rather than migration-ledger
//! managed: it must be visible to old binaries during a rolling deployment.

mod engine;
mod error;
mod record;
mod registry;
mod sql;

pub use engine::{
    adopt_if_matched, enroll, ensure_table, live_postgres_identity,
    live_postgres_identity_via_pg_pool, sqlite_pool_is_in_memory, verify, verify_via_control_plane,
    verify_via_pg_pool, verify_via_sqlite_pool, AdoptOutcome, AttestedLineage,
};
pub use error::LineageError;
pub use record::{
    DeploymentUuid, LineageAction, LineageRecord, LineageUuid, PgDatabaseIdentity, PgIdentity,
    PgSchemaIdentity, PgSystemIdentifier,
};
pub use registry::{
    ControlPlaneLineageAttestor, DatabaseLineageAttestor, DurableStore, LineageAttestor,
    LineageRegistry, LineageRegistryBuilder, LineageRegistryEntry, LineageReport, LineageStatus,
    LineageTopology, SqlxLineageAttestor,
};

#[cfg(test)]
mod tests;
