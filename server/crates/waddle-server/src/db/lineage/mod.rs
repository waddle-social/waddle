//! Durable database lineage attestation.
//!
//! `_lineage` is intentionally bootstrap-managed rather than migration-ledger
//! managed: it must be visible to old binaries during a rolling deployment.

mod engine;
mod error;
mod record;
mod registry;
mod sql;

pub use engine::{adopt, enroll, ensure_table, verify, AttestedLineage, LINEAGE_ADVISORY_LOCK_KEY};
pub use error::LineageError;
pub use record::{
    DeploymentUuid, LineageAction, LineageRecord, LineageUuid, PgDatabaseIdentity, PgIdentity,
    PgSchemaIdentity, PgSystemIdentifier,
};
pub use registry::{
    DatabaseLineageAttestor, DurableStore, LineageAttestor, LineageRegistry,
    LineageRegistryBuilder, LineageRegistryEntry, LineageReport, LineageStatus, LineageTopology,
};

#[cfg(test)]
mod tests;
