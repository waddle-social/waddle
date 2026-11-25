//! Structural tenant isolation runtime: `TenantRegistry`, `TursoTenantStore`,
//! `PostgresTenantStore`, `TursoPlacementLeaseStore`, `PostgresPlacementLeaseStore`,
//! SPIFFE X.509 SAN extraction, and `BootInstant` quiesce/drain controllers.

pub mod data_ops;
pub mod placement_postgres;
pub mod placement_turso;
pub mod postgres_store;
pub mod quiesce;
pub mod registry;
pub mod spiffe_x509;
pub mod turso_store;

pub use data_ops::{
    RemoteParticipantRecord, TenantAuthIdentityRecord, TenantDataOps, TenantJobRecord,
    TenantSpaceRecord, TenantUserRecord,
};
pub use placement_postgres::{
    assert_placement_lease_for_share, PostgresPlacementLeaseStore, SqliteCompatPlacementLeaseStore,
    ASSERT_PLACEMENT_LEASE_FOR_SHARE_SQL,
};
pub use placement_turso::{verify_turso_write_fence, TursoPlacementLeaseStore, MAX_LEASE_TTL_MS};
pub use postgres_store::{PostgresTenantStore, SqliteCompatPostgresStore};
pub use quiesce::{
    drain_timeout_from_env, HandoffOutcome, LocalLeaseGuard, PlacementTraceEmitter,
    QuiesceController, QuiesceError, DEFAULT_DRAIN_TIMEOUT_SECS,
};
pub use registry::{TenantHandleGuard, TenantRegistry, TenantRegistryError};
pub use spiffe_x509::{extract_spiffe_id_from_der, spiffe_matches_cell_role, SpiffeCertError};
pub use turso_store::{
    PinnedTursoConn, TursoPoolConfig, TursoTenantStore, DEFAULT_RESERVED_FDS, FDS_PER_TURSO_WAL_DB,
};
