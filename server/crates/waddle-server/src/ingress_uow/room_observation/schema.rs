use crate::config::LineageConfig;
use crate::db::lineage::{AttestedLineage, PgIdentity};
use crate::db::physical_attestation::{
    quote_identifier, validate_identifier, AttestedPostgresWriterCapability, PgRoleName,
};
use crate::db::{
    verify_room_observation_migration_v1023_in_transaction, Database, DatabaseDriver,
    DatabaseError, Transaction,
};
use std::collections::{BTreeMap, BTreeSet};

use super::ObservationError;

/// One owner-prepared schema revision for extension room observations.
pub const ROOM_OBSERVATION_SCHEMA_REVISION: u32 = 1;

/// The marker distinguishes owner preparation from legacy startup-created DDL.
pub const ROOM_OBSERVATION_OWNER_STATE_TABLE: &str = "extension_room_observation_owner_state";

/// Shared with the legacy initializer so its DDL and owner preparation serialize.
const ROOM_OBSERVATION_SCHEMA_LOCK: i64 = 6_316_133_403_883_761_747;
const ROOM_OBSERVATION_OWNER_GATE_LOCK: i64 = 0x7764_6c72_6f62_7331;

const ROOM_OBSERVATION_OWNER_STATE_SINGLETON_CHECK: &str =
    "extension_room_observation_owner_state_singleton_check";
const ROOM_OBSERVATION_OWNER_STATE_REVISION_CHECK: &str =
    "extension_room_observation_owner_state_revision_check";

/// Diagnostic evidence returned only after the owner marker is complete or
/// the attested runtime has verified that same marker and the full schema.
/// This value contains no database, transaction, repository, or serving handle.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RoomObservationSchemaEvidence {
    identity: PgIdentity,
    lineage: AttestedLineage,
    revision: u32,
}

impl RoomObservationSchemaEvidence {
    pub fn physical_identity(&self) -> &PgIdentity {
        &self.identity
    }

    pub fn attested_lineage(&self) -> &AttestedLineage {
        &self.lineage
    }

    pub fn revision(&self) -> u32 {
        self.revision
    }
}

/// Dark owner-preparation and exact attested-main-pool verification failures.
/// Database diagnostics are deliberately collapsed to an opaque error.
#[derive(Debug, thiserror::Error)]
pub enum RoomObservationSchemaError {
    #[error("room observation schema preparation requires PostgreSQL")]
    PostgresRequired,
    #[error("room observation schema preparation requires the main database lane")]
    MainLaneRequired,
    #[error("room observation owner preparation requires an unattested owner handle")]
    OwnerHandleRequired,
    #[error("room observation verification requires an attested PostgreSQL main pool")]
    AttestedPoolRequired,
    #[error("room observation schema identity does not match the supplied physical identity")]
    IdentityMismatch,
    #[error("room observation schema owner role or namespace is unsafe")]
    UnsafeOwner,
    #[error("room observation runtime role or grants are unsafe")]
    UnsafeRuntime,
    #[error("room observation owner marker is missing or incomplete")]
    OwnerPreparationIncomplete,
    #[error(
        "room observation owner commit outcome is unknown; reconcile with the attested verifier"
    )]
    CommitUnknown,
    #[error("room observation owner marker has an unexpected schema")]
    InvalidOwnerMarker,
    #[error("room observation schema has an unexpected table, column, constraint, index, or ACL")]
    InvalidSchemaShape,
    #[error("room observation database operation failed")]
    Database,
}

impl From<DatabaseError> for RoomObservationSchemaError {
    fn from(_: DatabaseError) -> Self {
        Self::Database
    }
}

/// Explicit startup initialization. These tables intentionally have no FK to
/// ingress envelope rows: cleanup of an archive source cannot erase pending
/// observation work or a result awaiting durable room publication.
pub async fn initialize_room_observations(db: &Database) -> Result<(), ObservationError> {
    let driver = db.driver();
    let mut tx = match driver {
        DatabaseDriver::Sqlite => db.begin_immediate().await?,
        DatabaseDriver::Postgres => db.begin().await?,
    };
    if driver == DatabaseDriver::Postgres {
        // Legacy initialization cooperates with owner preparation and the
        // read-only verifier before it takes the DDL lock.
        tx.execute("SET LOCAL lock_timeout = '5s'", ()).await?;
        owner_gate_lock(&mut tx, true).await?;
        schema_lock(&mut tx, false).await?;
    }
    apply_room_observation_schema(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

async fn apply_room_observation_schema(tx: &mut Transaction<'_>) -> Result<(), ObservationError> {
    tx.execute(
        r#"CREATE TABLE IF NOT EXISTS extension_room_observers (
    plugin_id TEXT PRIMARY KEY,
    generation BIGINT NOT NULL,
    identity TEXT NOT NULL,
    scope_json TEXT NOT NULL,
    max_concurrent BIGINT NOT NULL
)"#,
        (),
    )
    .await?;
    tx.execute(
        r#"CREATE TABLE IF NOT EXISTS extension_room_sources (
    source_key TEXT PRIMARY KEY,
    room_jid TEXT NOT NULL,
    sender_jid TEXT NOT NULL,
    root_stanza_id TEXT NOT NULL,
    revision_stanza_id TEXT NOT NULL,
    root_origin_id TEXT,
    revision BIGINT NOT NULL,
    source_json TEXT NOT NULL,
    retracted BIGINT NOT NULL DEFAULT 0,
    captured_at_ms BIGINT NOT NULL DEFAULT 0,
    UNIQUE (room_jid, root_stanza_id)
)"#,
        (),
    )
    .await?;
    tx.execute(
        r#"CREATE TABLE IF NOT EXISTS extension_room_source_revisions (
    room_jid TEXT NOT NULL,
    room_stanza_id TEXT NOT NULL,
    source_key TEXT NOT NULL,
    PRIMARY KEY (room_jid, room_stanza_id)
)"#,
        (),
    )
    .await?;
    tx.execute(
        r#"CREATE TABLE IF NOT EXISTS extension_room_observation_work (
    id TEXT PRIMARY KEY,
    source_key TEXT NOT NULL,
    message_key TEXT NOT NULL,
    plugin_id TEXT NOT NULL,
    generation BIGINT NOT NULL,
    identity TEXT NOT NULL,
    room_jid TEXT NOT NULL,
    revision BIGINT NOT NULL,
    source_json TEXT NOT NULL,
    body TEXT NOT NULL,
    status TEXT NOT NULL,
    attempt BIGINT NOT NULL DEFAULT 0,
    due_at_ms BIGINT NOT NULL,
    lease_id TEXT,
    lease_until_ms BIGINT,
    lease_node_id TEXT,
    lease_node_incarnation TEXT,
    terminal_category TEXT,
    usage_json TEXT,
    settled_at_ms BIGINT NULL,
    UNIQUE (plugin_id, generation, room_jid, source_key, revision)
)"#,
        (),
    )
    .await?;
    tx.execute(
        r#"CREATE INDEX IF NOT EXISTS extension_room_observation_work_due
    ON extension_room_observation_work(plugin_id, room_jid, status, due_at_ms)"#,
        (),
    )
    .await?;
    tx.execute(
        r#"CREATE TABLE IF NOT EXISTS extension_room_observation_receipts (
    plugin_id TEXT NOT NULL,
    generation BIGINT NOT NULL,
    room_jid TEXT NOT NULL,
    message_key TEXT NOT NULL,
    category TEXT NOT NULL,
    recorded_at_ms BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (plugin_id, generation, room_jid, message_key)
)"#,
        (),
    )
    .await?;
    tx.execute(
        r#"CREATE TABLE IF NOT EXISTS extension_room_publications (
    id TEXT PRIMARY KEY,
    work_id TEXT NOT NULL,
    output_index BIGINT NOT NULL,
    source_key TEXT NOT NULL,
    plugin_id TEXT NOT NULL,
    generation BIGINT NOT NULL,
    identity TEXT NOT NULL,
    room_jid TEXT NOT NULL,
    revision BIGINT NOT NULL,
    source_json TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    status TEXT NOT NULL,
    settled_at_ms BIGINT NULL,
    UNIQUE (work_id, output_index)
)"#,
        (),
    )
    .await?;
    tx.execute(
        r#"CREATE INDEX IF NOT EXISTS extension_room_publications_pending
    ON extension_room_publications(plugin_id, room_jid, status)"#,
        (),
    )
    .await?;
    Ok(())
}

#[derive(Clone, Copy)]
enum ColumnDefault {
    None,
    Zero,
}

#[derive(Clone, Copy)]
struct ColumnContract {
    name: &'static str,
    kind: &'static str,
    not_null: bool,
    default: ColumnDefault,
}

const fn column(name: &'static str, kind: &'static str, not_null: bool) -> ColumnContract {
    ColumnContract {
        name,
        kind,
        not_null,
        default: ColumnDefault::None,
    }
}

const fn zero_default_column(
    name: &'static str,
    kind: &'static str,
    not_null: bool,
) -> ColumnContract {
    ColumnContract {
        name,
        kind,
        not_null,
        default: ColumnDefault::Zero,
    }
}

#[derive(Clone, Copy)]
struct ConstraintContract {
    kind: &'static str,
    columns: &'static [&'static str],
    name: Option<&'static str>,
    normalized_check: Option<&'static str>,
}

const fn key(kind: &'static str, columns: &'static [&'static str]) -> ConstraintContract {
    ConstraintContract {
        kind,
        columns,
        name: None,
        normalized_check: None,
    }
}

const fn check(
    name: &'static str,
    columns: &'static [&'static str],
    normalized_check: &'static str,
) -> ConstraintContract {
    ConstraintContract {
        kind: "c",
        columns,
        name: Some(name),
        normalized_check: Some(normalized_check),
    }
}

#[derive(Clone, Copy)]
struct IndexContract {
    name: Option<&'static str>,
    unique: bool,
    primary: bool,
    columns: &'static [&'static str],
    predicate: Option<&'static str>,
}

const fn key_index(unique: bool, primary: bool, columns: &'static [&'static str]) -> IndexContract {
    IndexContract {
        name: None,
        unique,
        primary,
        columns,
        predicate: None,
    }
}

const fn named_index(name: &'static str, columns: &'static [&'static str]) -> IndexContract {
    IndexContract {
        name: Some(name),
        unique: false,
        primary: false,
        columns,
        predicate: None,
    }
}

const fn partial_index(
    name: &'static str,
    columns: &'static [&'static str],
    predicate: &'static str,
) -> IndexContract {
    IndexContract {
        name: Some(name),
        unique: false,
        primary: false,
        columns,
        predicate: Some(predicate),
    }
}

struct TableContract {
    name: &'static str,
    columns: &'static [ColumnContract],
    constraints: &'static [ConstraintContract],
    indexes: &'static [IndexContract],
    read_only: bool,
}

const OBSERVERS_COLUMNS: &[ColumnContract] = &[
    column("plugin_id", "text", true),
    column("generation", "bigint", true),
    column("identity", "text", true),
    column("scope_json", "text", true),
    column("max_concurrent", "bigint", true),
];
const OBSERVERS_CONSTRAINTS: &[ConstraintContract] = &[key("p", &["plugin_id"])];
const OBSERVERS_INDEXES: &[IndexContract] = &[key_index(true, true, &["plugin_id"])];

const SOURCES_COLUMNS: &[ColumnContract] = &[
    column("source_key", "text", true),
    column("room_jid", "text", true),
    column("sender_jid", "text", true),
    column("root_stanza_id", "text", true),
    column("revision_stanza_id", "text", true),
    column("root_origin_id", "text", false),
    column("revision", "bigint", true),
    column("source_json", "text", true),
    zero_default_column("retracted", "bigint", true),
    zero_default_column("captured_at_ms", "bigint", true),
];
const SOURCES_CONSTRAINTS: &[ConstraintContract] = &[
    key("p", &["source_key"]),
    key("u", &["room_jid", "root_stanza_id"]),
];
const SOURCES_INDEXES: &[IndexContract] = &[
    key_index(true, true, &["source_key"]),
    key_index(true, false, &["room_jid", "root_stanza_id"]),
    partial_index(
        "extension_room_sources_retracted_captured",
        &["captured_at_ms", "source_key"],
        "retracted=1",
    ),
];

const SOURCE_REVISIONS_COLUMNS: &[ColumnContract] = &[
    column("room_jid", "text", true),
    column("room_stanza_id", "text", true),
    column("source_key", "text", true),
];
const SOURCE_REVISIONS_CONSTRAINTS: &[ConstraintContract] =
    &[key("p", &["room_jid", "room_stanza_id"])];
const SOURCE_REVISIONS_INDEXES: &[IndexContract] = &[
    key_index(true, true, &["room_jid", "room_stanza_id"]),
    named_index(
        "extension_room_source_revisions_source",
        &["source_key", "room_jid", "room_stanza_id"],
    ),
];

const WORK_COLUMNS: &[ColumnContract] = &[
    column("id", "text", true),
    column("source_key", "text", true),
    column("message_key", "text", true),
    column("plugin_id", "text", true),
    column("generation", "bigint", true),
    column("identity", "text", true),
    column("room_jid", "text", true),
    column("revision", "bigint", true),
    column("source_json", "text", true),
    column("body", "text", true),
    column("status", "text", true),
    zero_default_column("attempt", "bigint", true),
    column("due_at_ms", "bigint", true),
    column("lease_id", "text", false),
    column("lease_until_ms", "bigint", false),
    column("lease_node_id", "text", false),
    column("lease_node_incarnation", "text", false),
    column("terminal_category", "text", false),
    column("usage_json", "text", false),
    column("settled_at_ms", "bigint", false),
];
const WORK_CONSTRAINTS: &[ConstraintContract] = &[
    key("p", &["id"]),
    key(
        "u",
        &[
            "plugin_id",
            "generation",
            "room_jid",
            "source_key",
            "revision",
        ],
    ),
];
const WORK_INDEXES: &[IndexContract] = &[
    key_index(true, true, &["id"]),
    key_index(
        true,
        false,
        &[
            "plugin_id",
            "generation",
            "room_jid",
            "source_key",
            "revision",
        ],
    ),
    named_index(
        "extension_room_observation_work_due",
        &["plugin_id", "room_jid", "status", "due_at_ms"],
    ),
    partial_index(
        "extension_room_observation_work_settled",
        &["settled_at_ms", "id"],
        "status=anyarray['completed','terminal','stale']",
    ),
    named_index("extension_room_observation_work_source", &["source_key"]),
    partial_index(
        "extension_room_observation_work_active_guard",
        &["plugin_id", "generation", "room_jid", "message_key"],
        "status=anyarray['pending','leased','started']",
    ),
];

const RECEIPTS_COLUMNS: &[ColumnContract] = &[
    column("plugin_id", "text", true),
    column("generation", "bigint", true),
    column("room_jid", "text", true),
    column("message_key", "text", true),
    column("category", "text", true),
    zero_default_column("recorded_at_ms", "bigint", true),
];
const RECEIPTS_CONSTRAINTS: &[ConstraintContract] = &[key(
    "p",
    &["plugin_id", "generation", "room_jid", "message_key"],
)];
const RECEIPTS_INDEXES: &[IndexContract] = &[
    key_index(
        true,
        true,
        &["plugin_id", "generation", "room_jid", "message_key"],
    ),
    named_index(
        "extension_room_observation_receipts_recorded",
        &[
            "recorded_at_ms",
            "plugin_id",
            "generation",
            "room_jid",
            "message_key",
        ],
    ),
];

const PUBLICATIONS_COLUMNS: &[ColumnContract] = &[
    column("id", "text", true),
    column("work_id", "text", true),
    column("output_index", "bigint", true),
    column("source_key", "text", true),
    column("plugin_id", "text", true),
    column("generation", "bigint", true),
    column("identity", "text", true),
    column("room_jid", "text", true),
    column("revision", "bigint", true),
    column("source_json", "text", true),
    column("payload_json", "text", true),
    column("status", "text", true),
    column("settled_at_ms", "bigint", false),
];
const PUBLICATIONS_CONSTRAINTS: &[ConstraintContract] =
    &[key("p", &["id"]), key("u", &["work_id", "output_index"])];
const PUBLICATIONS_INDEXES: &[IndexContract] = &[
    key_index(true, true, &["id"]),
    key_index(true, false, &["work_id", "output_index"]),
    named_index(
        "extension_room_publications_pending",
        &["plugin_id", "room_jid", "status"],
    ),
    partial_index(
        "extension_room_publications_settled",
        &["settled_at_ms", "id"],
        "status=anyarray['published','stale']",
    ),
    named_index("extension_room_publications_source", &["source_key"]),
];

const OWNER_STATE_COLUMNS: &[ColumnContract] = &[
    column("singleton", "text", true),
    column("prepared_revision", "text", true),
    column("deployment_uuid", "text", true),
    column("lineage_uuid", "text", true),
    column("system_identifier", "text", true),
    column("database_oid", "bigint", true),
    column("database_name", "text", true),
    column("schema_oid", "bigint", true),
    column("schema_name", "text", true),
    column("schema_owner_oid", "bigint", true),
    column("schema_owner_name", "text", true),
    column("schema_owner_member_oid", "bigint", true),
    column("schema_owner_member_name", "text", true),
];
const OWNER_STATE_CONSTRAINTS: &[ConstraintContract] = &[
    key("p", &["singleton"]),
    check(
        ROOM_OBSERVATION_OWNER_STATE_SINGLETON_CHECK,
        &["singleton"],
        "singleton='1'",
    ),
    check(
        ROOM_OBSERVATION_OWNER_STATE_REVISION_CHECK,
        &["prepared_revision"],
        "prepared_revision=anyarray['0','1']",
    ),
];
const OWNER_STATE_INDEXES: &[IndexContract] = &[key_index(true, true, &["singleton"])];

const DATA_TABLES: &[TableContract] = &[
    TableContract {
        name: "extension_room_observers",
        columns: OBSERVERS_COLUMNS,
        constraints: OBSERVERS_CONSTRAINTS,
        indexes: OBSERVERS_INDEXES,
        read_only: false,
    },
    TableContract {
        name: "extension_room_sources",
        columns: SOURCES_COLUMNS,
        constraints: SOURCES_CONSTRAINTS,
        indexes: SOURCES_INDEXES,
        read_only: false,
    },
    TableContract {
        name: "extension_room_source_revisions",
        columns: SOURCE_REVISIONS_COLUMNS,
        constraints: SOURCE_REVISIONS_CONSTRAINTS,
        indexes: SOURCE_REVISIONS_INDEXES,
        read_only: false,
    },
    TableContract {
        name: "extension_room_observation_work",
        columns: WORK_COLUMNS,
        constraints: WORK_CONSTRAINTS,
        indexes: WORK_INDEXES,
        read_only: false,
    },
    TableContract {
        name: "extension_room_observation_receipts",
        columns: RECEIPTS_COLUMNS,
        constraints: RECEIPTS_CONSTRAINTS,
        indexes: RECEIPTS_INDEXES,
        read_only: false,
    },
    TableContract {
        name: "extension_room_publications",
        columns: PUBLICATIONS_COLUMNS,
        constraints: PUBLICATIONS_CONSTRAINTS,
        indexes: PUBLICATIONS_INDEXES,
        read_only: false,
    },
];

const OWNER_STATE_CONTRACT: TableContract = TableContract {
    name: ROOM_OBSERVATION_OWNER_STATE_TABLE,
    columns: OWNER_STATE_COLUMNS,
    constraints: OWNER_STATE_CONSTRAINTS,
    indexes: OWNER_STATE_INDEXES,
    read_only: true,
};

/// Prepare the PostgreSQL room-observation owner marker and grants through an
/// operator-selected schema-owner handle. The caller supplies the expected
/// attested lineage; the function does not run migrations, create or repair
/// the six data tables, synchronize configured observers, start actors, or
/// return a serving handle.
///
/// `designated_migrator` is an external trust input. Its login must be the
/// session user and the sole SET-only member of the NOLOGIN schema owner role.
/// `attestation_definer` is the externally configured physical-attestation
/// definer. It must already have USAGE on the target schema and SELECT on
/// `_lineage`; the schema grant must come from the schema owner. This function
/// checks those effective privileges and does not change either grant.
/// Before calling this function, the operator must grant that owner role
/// EXECUTE on `pg_catalog.pg_control_system()`; this function never changes
/// that system-function ACL.
pub async fn prepare_dark_room_observation_schema(
    db: &Database,
    expected: &AttestedLineage,
    designated_migrator: &PgRoleName,
    attestation_definer: &PgRoleName,
    runtime: &PgRoleName,
) -> Result<RoomObservationSchemaEvidence, RoomObservationSchemaError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(RoomObservationSchemaError::PostgresRequired);
    }
    if !db.is_main_lane() {
        return Err(RoomObservationSchemaError::MainLaneRequired);
    }
    if db.attested_pg_writer_capability().is_some() {
        return Err(RoomObservationSchemaError::OwnerHandleRequired);
    }
    validate_identifier(designated_migrator.as_str(), "room observation migrator")
        .map_err(|_| RoomObservationSchemaError::UnsafeOwner)?;
    validate_identifier(attestation_definer.as_str(), "physical attestation definer")
        .map_err(|_| RoomObservationSchemaError::UnsafeRuntime)?;
    if attestation_definer == designated_migrator || attestation_definer == runtime {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    }
    let identity = expected
        .postgres_identity
        .as_ref()
        .ok_or(RoomObservationSchemaError::IdentityMismatch)?;
    validate_expected_identity(identity)?;
    let mut tx = db.begin().await?;
    tx.execute("SET TRANSACTION ISOLATION LEVEL READ COMMITTED", ())
        .await?;
    set_transaction_timeouts(&mut tx).await?;
    set_target_search_path(&mut tx, &identity.schema.name.0).await?;
    lineage_lock(&mut tx, false).await?;
    verify_room_observation_migration_v1023_in_transaction(&mut tx, &identity.schema.name.0)
        .await?;
    owner_gate_lock(&mut tx, false).await?;
    schema_lock(&mut tx, false).await?;
    lock_data_tables(&mut tx, identity).await?;
    verify_owner_boundary(&mut tx, identity, designated_migrator.as_str()).await?;
    verify_owner_lineage(&mut tx, expected, identity).await?;
    verify_data_catalog_shape(&mut tx, identity).await?;
    verify_pg_monitor_grants(&mut tx, identity).await?;
    prepare_incomplete_marker(&mut tx, expected, identity, designated_migrator.as_str()).await?;
    grant_room_observation_runtime(&mut tx, identity, runtime).await?;
    verify_room_observation_schema_shape(
        &mut tx,
        expected,
        identity,
        designated_migrator.as_str(),
        ObservationVerification {
            attestation_definer: attestation_definer.as_str(),
            runtime: runtime.as_str(),
            marker_revision: "0",
            profile: None,
        },
    )
    .await?;
    let marker = qualified_table(&identity.schema.name.0, ROOM_OBSERVATION_OWNER_STATE_TABLE);
    let updated = tx
        .execute(
            &format!(
                "UPDATE {marker} SET prepared_revision = '1' \
                 WHERE singleton = '1' AND prepared_revision = '0' \
                   AND deployment_uuid = ? AND lineage_uuid = ? \
                   AND system_identifier = ? AND database_oid = ? \
                   AND database_name = ? AND schema_oid = ? AND schema_name = ?"
            ),
            crate::db_params![
                expected.deployment_uuid.to_string(),
                expected.lineage_uuid.to_string(),
                identity.system_identifier.to_string(),
                i64::from(identity.database.oid),
                &identity.database.name.0,
                i64::from(identity.schema.oid),
                &identity.schema.name.0,
            ],
        )
        .await?;
    if updated != 1 {
        return Err(RoomObservationSchemaError::OwnerPreparationIncomplete);
    }
    tx.commit()
        .await
        .map_err(|_| RoomObservationSchemaError::CommitUnknown)?;

    Ok(RoomObservationSchemaEvidence {
        identity: identity.clone(),
        lineage: expected.clone(),
        revision: ROOM_OBSERVATION_SCHEMA_REVISION,
    })
}

/// Verify the complete owner-prepared schema on the exact physically
/// attested PostgreSQL main pool. This function is read-only and returns only
/// opaque completion evidence, never a repository or worker handle.
/// `designated_migrator` must match the externally configured login recorded
/// during owner preparation and must remain the sole SET-only member of the
/// schema owner role.
pub async fn verify_attested_room_observation_schema(
    db: &Database,
    expected: &AttestedLineage,
    designated_migrator: &PgRoleName,
) -> Result<RoomObservationSchemaEvidence, RoomObservationSchemaError> {
    verify_attested_room_observation_schema_inner(db, expected, designated_migrator, None).await
}

/// Diagnose the composed A/H role profile without selecting a store or worker.
/// The supplied OIDs must come from an independent provisioning handoff.
pub async fn verify_attested_room_observation_schema_v1(
    db: &Database,
    expected: &AttestedLineage,
    designated_migrator: &PgRoleName,
    profile: &crate::pubsub::RestrictedServingProfileV1,
) -> Result<RoomObservationSchemaEvidence, RoomObservationSchemaError> {
    verify_attested_room_observation_schema_inner(db, expected, designated_migrator, Some(profile))
        .await
}

async fn verify_attested_room_observation_schema_inner(
    db: &Database,
    expected: &AttestedLineage,
    designated_migrator: &PgRoleName,
    profile: Option<&crate::pubsub::RestrictedServingProfileV1>,
) -> Result<RoomObservationSchemaEvidence, RoomObservationSchemaError> {
    if db.driver() != DatabaseDriver::Postgres {
        return Err(RoomObservationSchemaError::PostgresRequired);
    }
    if !db.is_main_lane() {
        return Err(RoomObservationSchemaError::MainLaneRequired);
    }
    let capability = db
        .attested_pg_writer_capability()
        .ok_or(RoomObservationSchemaError::AttestedPoolRequired)?;
    db.verify_attested_pg_writer_capability(&capability)
        .map_err(|_| RoomObservationSchemaError::AttestedPoolRequired)?;
    let identity = capability.identity().clone();
    validate_expected_identity(&identity)?;
    if capability
        .attested_lineage_for_deployment(Some(expected.deployment_uuid))
        .map_err(|_| RoomObservationSchemaError::IdentityMismatch)?
        != *expected
    {
        return Err(RoomObservationSchemaError::IdentityMismatch);
    }
    if expected.postgres_identity.as_ref() != Some(&identity) {
        return Err(RoomObservationSchemaError::IdentityMismatch);
    }
    validate_identifier(designated_migrator.as_str(), "room observation migrator")
        .map_err(|_| RoomObservationSchemaError::UnsafeOwner)?;

    let mut tx = db.begin().await?;
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await?;
    // READ COMMITTED is intentional: the following shared advisory-lock
    // statement may wait for owner preparation. REPEATABLE READ could pin a
    // stale snapshot before that wait and then inspect the old marker.
    crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
        .await
        .map_err(|_| RoomObservationSchemaError::Database)?;
    set_target_search_path(&mut tx, &identity.schema.name.0).await?;
    lineage_lock(&mut tx, true).await?;
    verify_room_observation_migration_v1023_in_transaction(&mut tx, &identity.schema.name.0)
        .await?;
    owner_gate_lock(&mut tx, true).await?;
    schema_lock(&mut tx, true).await?;
    for table in std::iter::once(&OWNER_STATE_CONTRACT).chain(DATA_TABLES.iter()) {
        let qualified = qualified_table(&identity.schema.name.0, table.name);
        tx.execute(&format!("LOCK TABLE {qualified} IN ACCESS SHARE MODE"), ())
            .await
            .map_err(|_| RoomObservationSchemaError::OwnerPreparationIncomplete)?;
    }

    capability
        .attest_transaction(&mut tx)
        .await
        .map_err(|_| RoomObservationSchemaError::IdentityMismatch)?;
    verify_attested_lineage(&mut tx, &capability, expected, &identity).await?;
    verify_active_schema(&mut tx, &identity).await?;
    if profile.is_some() {
        tx.execute("SET LOCAL search_path = pg_catalog, pg_temp", ())
            .await?;
    }
    verify_owner_marker(
        &mut tx,
        expected,
        &identity,
        designated_migrator.as_str(),
        Some("1"),
    )
    .await?;
    let runtime = runtime_login_from_transaction(&mut tx).await?;
    verify_room_observation_schema_shape(
        &mut tx,
        expected,
        &identity,
        designated_migrator.as_str(),
        ObservationVerification {
            attestation_definer: capability.definer_role().as_str(),
            runtime: &runtime,
            marker_revision: "1",
            profile,
        },
    )
    .await?;
    tx.rollback().await?;

    Ok(RoomObservationSchemaEvidence {
        identity,
        lineage: expected.clone(),
        revision: ROOM_OBSERVATION_SCHEMA_REVISION,
    })
}

#[cfg(all(test, feature = "test-support"))]
#[path = "schema_tests.rs"]
mod tests;

async fn schema_lock(tx: &mut Transaction<'_>, shared: bool) -> Result<(), DatabaseError> {
    let sql = if shared {
        "SELECT pg_catalog.pg_advisory_xact_lock_shared(?)"
    } else {
        "SELECT pg_catalog.pg_advisory_xact_lock(?)"
    };
    let mut rows = tx
        .query(sql, crate::db_params![ROOM_OBSERVATION_SCHEMA_LOCK])
        .await?;
    rows.next().await?.ok_or(DatabaseError::QueryFailed(
        "room observation schema lock returned no row".to_owned(),
    ))?;
    Ok(())
}

async fn lineage_lock(tx: &mut Transaction<'_>, shared: bool) -> Result<(), DatabaseError> {
    let sql = if shared {
        "SELECT pg_catalog.pg_advisory_xact_lock_shared(?)"
    } else {
        "SELECT pg_catalog.pg_advisory_xact_lock(?)"
    };
    let mut rows = tx
        .query(
            sql,
            crate::db_params![crate::db::lineage::LINEAGE_ADVISORY_LOCK_KEY],
        )
        .await?;
    rows.next().await?.ok_or(DatabaseError::QueryFailed(
        "room observation lineage lock returned no row".to_owned(),
    ))?;
    Ok(())
}

async fn lock_data_tables(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
) -> Result<(), RoomObservationSchemaError> {
    for table in DATA_TABLES {
        let qualified = qualified_table(&identity.schema.name.0, table.name);
        tx.execute(&format!("LOCK TABLE {qualified} IN ACCESS SHARE MODE"), ())
            .await
            .map_err(|_| RoomObservationSchemaError::InvalidSchemaShape)?;
    }
    Ok(())
}

async fn owner_gate_lock(tx: &mut Transaction<'_>, shared: bool) -> Result<(), DatabaseError> {
    let sql = if shared {
        "SELECT pg_catalog.pg_advisory_xact_lock_shared(?)"
    } else {
        "SELECT pg_catalog.pg_advisory_xact_lock(?)"
    };
    let mut rows = tx
        .query(sql, crate::db_params![ROOM_OBSERVATION_OWNER_GATE_LOCK])
        .await?;
    rows.next().await?.ok_or(DatabaseError::QueryFailed(
        "room observation owner gate returned no row".to_owned(),
    ))?;
    Ok(())
}

fn validate_expected_identity(identity: &PgIdentity) -> Result<(), RoomObservationSchemaError> {
    validate_identifier(&identity.schema.name.0, "room observation schema")
        .map_err(|_| RoomObservationSchemaError::IdentityMismatch)?;
    if identity.database.oid == 0 || identity.schema.oid == 0 {
        return Err(RoomObservationSchemaError::IdentityMismatch);
    }
    Ok(())
}

async fn set_transaction_timeouts(
    tx: &mut Transaction<'_>,
) -> Result<(), RoomObservationSchemaError> {
    tx.execute("SET LOCAL lock_timeout = '3s'", ()).await?;
    tx.execute("SET LOCAL statement_timeout = '30s'", ())
        .await?;
    Ok(())
}

async fn set_target_search_path(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), RoomObservationSchemaError> {
    // Omitting pg_catalog keeps PostgreSQL's implicit catalog-first lookup
    // ahead of owner-controlled operators and functions while making the
    // target schema current for the migration ledger verifier.
    let path = quote_identifier(schema);
    let mut rows = tx
        .query(
            "SELECT pg_catalog.set_config('search_path', ?, true)",
            crate::db_params![path],
        )
        .await?;
    rows.next()
        .await?
        .ok_or(RoomObservationSchemaError::IdentityMismatch)?;
    Ok(())
}

fn qualified_table(schema: &str, table: &str) -> String {
    format!("{}.\"{table}\"", quote_identifier(schema))
}

async fn verify_owner_boundary(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
    designated_migrator: &str,
) -> Result<(), RoomObservationSchemaError> {
    let mut rows = tx
        .query(
            "SELECT (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
             database.oid::bigint, database.datname::text, namespace.oid::bigint, \
             namespace.nspname::text, namespace.nspname = pg_catalog.current_schema() \
               AND NOT pg_catalog.pg_is_in_recovery() \
               AND owner.rolname = current_user AND NOT owner.rolcanlogin \
               AND NOT owner.rolsuper AND NOT owner.rolcreatedb AND NOT owner.rolcreaterole \
               AND NOT owner.rolreplication AND NOT owner.rolbypassrls \
               AND owner.oid <> database.datdba \
               AND NOT pg_catalog.has_database_privilege(owner.oid, database.oid, 'CREATE,TEMP') \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership \
                               WHERE membership.member = owner.oid) \
               AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_auth_members membership \
                    WHERE membership.roleid = owner.oid) = 1 \
               AND EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership \
                           JOIN pg_catalog.pg_roles migrator ON migrator.oid = membership.member \
                           WHERE membership.roleid = owner.oid \
                             AND migrator.rolname = session_user \
                             AND migrator.rolname = ? \
                             AND migrator.rolcanlogin AND NOT migrator.rolsuper \
                             AND NOT migrator.rolcreatedb AND NOT migrator.rolcreaterole \
                             AND NOT migrator.rolreplication AND NOT migrator.rolbypassrls \
                             AND migrator.oid <> database.datdba \
                             AND NOT pg_catalog.has_database_privilege( \
                               migrator.oid, database.oid, 'CREATE,TEMP') \
                             AND NOT pg_catalog.has_schema_privilege( \
                               migrator.oid, namespace.oid, 'CREATE') \
                             AND membership.set_option AND NOT membership.inherit_option \
                             AND NOT membership.admin_option \
                             AND NOT EXISTS (SELECT 1 \
                                             FROM pg_catalog.pg_auth_members inbound \
                                             WHERE inbound.roleid = migrator.oid) \
                             AND pg_catalog.pg_has_role(migrator.oid, owner.oid, 'SET') \
                             AND NOT pg_catalog.pg_has_role(migrator.oid, owner.oid, 'USAGE') \
                             AND (SELECT pg_catalog.count(*) \
                                  FROM pg_catalog.pg_auth_members member_of \
                                  WHERE member_of.member = migrator.oid) = 1) \
               AND pg_catalog.current_setting('session_replication_role') = 'origin' \
               AND pg_catalog.pg_my_temp_schema() = 0 \
             FROM pg_catalog.pg_namespace namespace \
             JOIN pg_catalog.pg_roles owner ON owner.oid = namespace.nspowner \
             JOIN pg_catalog.pg_database database \
               ON database.datname = pg_catalog.current_database() \
             WHERE namespace.nspname = ?",
            crate::db_params![designated_migrator, &identity.schema.name.0],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(RoomObservationSchemaError::IdentityMismatch);
    };
    let valid = row.get::<String>(0)? == identity.system_identifier.to_string()
        && row.get::<i64>(1)? == i64::from(identity.database.oid)
        && row.get::<String>(2)? == identity.database.name.0
        && row.get::<i64>(3)? == i64::from(identity.schema.oid)
        && row.get::<String>(4)? == identity.schema.name.0
        && row.get::<bool>(5)?;
    if !valid {
        return Err(RoomObservationSchemaError::UnsafeOwner);
    }
    Ok(())
}

async fn verify_owner_lineage(
    tx: &mut Transaction<'_>,
    expected: &AttestedLineage,
    identity: &PgIdentity,
) -> Result<(), RoomObservationSchemaError> {
    let config = LineageConfig {
        deployment_uuid: Some(expected.deployment_uuid),
        action: None,
    };
    let table = format!("{}.\"_lineage\"", quote_identifier(&identity.schema.name.0));
    let observed = crate::db::lineage::verify_qualified_in_transaction(
        tx,
        DatabaseDriver::Postgres,
        &config,
        &table,
    )
    .await
    .map_err(|_| RoomObservationSchemaError::IdentityMismatch)?;
    if &observed != expected {
        return Err(RoomObservationSchemaError::IdentityMismatch);
    }
    Ok(())
}

async fn verify_data_catalog_shape(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
) -> Result<(), RoomObservationSchemaError> {
    for table in DATA_TABLES {
        verify_table_shape(tx, &identity.schema.name.0, table).await?;
    }
    Ok(())
}

async fn verify_pg_monitor_grants(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
) -> Result<(), RoomObservationSchemaError> {
    for table in DATA_TABLES {
        let mut rows = tx
            .query(
                "SELECT pg_catalog.has_table_privilege(monitor.oid, relation.oid, 'SELECT') \
                   AND NOT pg_catalog.has_table_privilege( \
                     monitor.oid, relation.oid, \
                     'INSERT, UPDATE, DELETE, TRUNCATE, REFERENCES, TRIGGER') \
                   AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000 \
                     THEN NOT pg_catalog.has_table_privilege(monitor.oid, relation.oid, 'MAINTAIN') \
                     ELSE true END \
                   AND (SELECT pg_catalog.count(*) FROM pg_catalog.aclexplode( \
                     COALESCE(relation.relacl, pg_catalog.acldefault('r', relation.relowner))) acl \
                     WHERE acl.grantee = monitor.oid) = 1 \
                   AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                     COALESCE(relation.relacl, pg_catalog.acldefault('r', relation.relowner))) acl \
                     WHERE acl.grantee = monitor.oid \
                       AND (acl.grantor <> relation.relowner OR acl.is_grantable \
                         OR acl.privilege_type <> 'SELECT')) \
                 FROM pg_catalog.pg_class relation \
                 JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
                 JOIN pg_catalog.pg_roles monitor ON monitor.rolname = 'pg_monitor' \
                 WHERE namespace.nspname = ? AND relation.relname = ? \
                   AND relation.relkind = 'r'",
                crate::db_params![&identity.schema.name.0, table.name],
            )
            .await?;
        let Some(row) = rows.next().await? else {
            return Err(RoomObservationSchemaError::InvalidSchemaShape);
        };
        if !row.get::<bool>(0)? {
            return Err(RoomObservationSchemaError::InvalidSchemaShape);
        }
    }
    Ok(())
}

async fn prepare_incomplete_marker(
    tx: &mut Transaction<'_>,
    expected: &AttestedLineage,
    identity: &PgIdentity,
    designated_migrator: &str,
) -> Result<(), RoomObservationSchemaError> {
    let schema = &identity.schema.name.0;
    let mut rows = tx
        .query(
            "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = ? AND relation.relname = ?)",
            crate::db_params![schema, ROOM_OBSERVATION_OWNER_STATE_TABLE],
        )
        .await?;
    let exists = rows
        .next()
        .await?
        .ok_or(RoomObservationSchemaError::InvalidOwnerMarker)?
        .get::<bool>(0)?;
    drop(rows);

    let marker = qualified_table(schema, ROOM_OBSERVATION_OWNER_STATE_TABLE);
    if !exists {
        tx.execute(
            &format!(
                "CREATE TABLE {marker} (\
                   singleton TEXT NOT NULL CONSTRAINT {ROOM_OBSERVATION_OWNER_STATE_SINGLETON_CHECK} \
                     CHECK (singleton = '1'), \
                   prepared_revision TEXT NOT NULL CONSTRAINT {ROOM_OBSERVATION_OWNER_STATE_REVISION_CHECK} \
                     CHECK (prepared_revision IN ('0', '1')), \
                   deployment_uuid TEXT NOT NULL, lineage_uuid TEXT NOT NULL, \
                   system_identifier TEXT NOT NULL, database_oid BIGINT NOT NULL, \
                   database_name TEXT NOT NULL, schema_oid BIGINT NOT NULL, \
                   schema_name TEXT NOT NULL, schema_owner_oid BIGINT NOT NULL, \
                   schema_owner_name TEXT NOT NULL, \
                   schema_owner_member_oid BIGINT NOT NULL, \
                   schema_owner_member_name TEXT NOT NULL, PRIMARY KEY (singleton))"
            ),
            (),
        )
        .await?;
        let inserted = tx
            .execute(
                &format!(
                    "INSERT INTO {marker} (singleton, prepared_revision, deployment_uuid, \
                 lineage_uuid, system_identifier, database_oid, database_name, schema_oid, \
                 schema_name, schema_owner_oid, schema_owner_name, \
                 schema_owner_member_oid, schema_owner_member_name) \
                 SELECT '1', '0', ?, ?, ?, ?, ?, ?, ?, owner.oid::bigint, owner.rolname::text, \
                 migrator.oid::bigint, migrator.rolname::text \
                 FROM pg_catalog.pg_namespace namespace \
                 JOIN pg_catalog.pg_roles owner ON owner.oid = namespace.nspowner \
                 JOIN pg_catalog.pg_auth_members membership ON membership.roleid = owner.oid \
                 JOIN pg_catalog.pg_roles migrator ON migrator.oid = membership.member \
                 WHERE namespace.nspname = ? AND migrator.rolname = ?"
                ),
                crate::db_params![
                    expected.deployment_uuid.to_string(),
                    expected.lineage_uuid.to_string(),
                    identity.system_identifier.to_string(),
                    i64::from(identity.database.oid),
                    &identity.database.name.0,
                    i64::from(identity.schema.oid),
                    schema,
                    schema,
                    designated_migrator,
                ],
            )
            .await?;
        if inserted != 1 {
            return Err(RoomObservationSchemaError::UnsafeOwner);
        }
    }

    verify_table_shape(tx, schema, &OWNER_STATE_CONTRACT).await?;
    verify_owner_marker(tx, expected, identity, designated_migrator, None).await?;
    let updated = tx
        .execute(
            &format!(
                "UPDATE {marker} SET prepared_revision = '0' \
                 WHERE singleton = '1' AND deployment_uuid = ? AND lineage_uuid = ? \
                   AND system_identifier = ? AND database_oid = ? AND database_name = ? \
                   AND schema_oid = ? AND schema_name = ?"
            ),
            crate::db_params![
                expected.deployment_uuid.to_string(),
                expected.lineage_uuid.to_string(),
                identity.system_identifier.to_string(),
                i64::from(identity.database.oid),
                &identity.database.name.0,
                i64::from(identity.schema.oid),
                schema,
            ],
        )
        .await?;
    if updated != 1 {
        return Err(RoomObservationSchemaError::OwnerPreparationIncomplete);
    }
    Ok(())
}

async fn verify_owner_marker(
    tx: &mut Transaction<'_>,
    expected: &AttestedLineage,
    identity: &PgIdentity,
    designated_migrator: &str,
    expected_revision: Option<&str>,
) -> Result<(), RoomObservationSchemaError> {
    let marker = qualified_table(&identity.schema.name.0, ROOM_OBSERVATION_OWNER_STATE_TABLE);
    let mut rows = tx
        .query(
            &format!(
                "SELECT marker.singleton, marker.prepared_revision, marker.deployment_uuid, \
                 marker.lineage_uuid, marker.system_identifier, marker.database_oid, \
                 marker.database_name, marker.schema_oid, marker.schema_name, \
                 marker.schema_owner_oid, marker.schema_owner_name, \
                 namespace.nspowner::bigint, owner.rolname::text, \
                 marker.schema_owner_member_oid, marker.schema_owner_member_name, \
                 (SELECT pg_catalog.count(*)::bigint \
                    FROM pg_catalog.pg_auth_members membership \
                    WHERE membership.roleid = namespace.nspowner), \
                 (SELECT membership.member::bigint \
                    FROM pg_catalog.pg_auth_members membership \
                    WHERE membership.roleid = namespace.nspowner \
                    ORDER BY membership.member LIMIT 1), \
                 (SELECT member_role.rolname::text \
                    FROM pg_catalog.pg_auth_members membership \
                    JOIN pg_catalog.pg_roles member_role ON member_role.oid = membership.member \
                    WHERE membership.roleid = namespace.nspowner \
                    ORDER BY membership.member LIMIT 1) \
                 FROM {marker} marker \
                 JOIN pg_catalog.pg_namespace namespace \
                   ON namespace.nspname = ? \
                 JOIN pg_catalog.pg_roles owner ON owner.oid = namespace.nspowner"
            ),
            crate::db_params![&identity.schema.name.0],
        )
        .await
        .map_err(|_| RoomObservationSchemaError::OwnerPreparationIncomplete)?;
    let Some(row) = rows.next().await? else {
        return Err(RoomObservationSchemaError::OwnerPreparationIncomplete);
    };
    let singleton: String = row.get(0)?;
    let revision: String = row.get(1)?;
    let lineage_matches = row.get::<String>(2)? == expected.deployment_uuid.to_string()
        && row.get::<String>(3)? == expected.lineage_uuid.to_string();
    let identity_matches = row.get::<String>(4)? == identity.system_identifier.to_string()
        && row.get::<i64>(5)? == i64::from(identity.database.oid)
        && row.get::<String>(6)? == identity.database.name.0
        && row.get::<i64>(7)? == i64::from(identity.schema.oid)
        && row.get::<String>(8)? == identity.schema.name.0;
    let owner_matches = row.get::<i64>(9)? == row.get::<i64>(11)?
        && row.get::<String>(10)? == row.get::<String>(12)?;
    let owner_member_oid: i64 = row.get(13)?;
    let owner_member_name: String = row.get(14)?;
    let member_count: i64 = row.get(15)?;
    let current_member_oid: Option<i64> = row.get(16)?;
    let current_member_name: Option<String> = row.get(17)?;
    let owner_member_matches = member_count == 1
        && owner_member_name == designated_migrator
        && Some(owner_member_oid) == current_member_oid
        && Some(owner_member_name) == current_member_name;
    if rows.next().await?.is_some() || singleton != "1" {
        return Err(RoomObservationSchemaError::InvalidOwnerMarker);
    }
    if !lineage_matches || !identity_matches || !owner_matches || !owner_member_matches {
        return Err(RoomObservationSchemaError::IdentityMismatch);
    }
    match expected_revision {
        Some(expected) if revision != expected => {
            Err(RoomObservationSchemaError::OwnerPreparationIncomplete)
        }
        None if revision != "0" && revision != "1" => {
            Err(RoomObservationSchemaError::InvalidOwnerMarker)
        }
        _ => Ok(()),
    }
}

async fn grant_room_observation_runtime(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
    runtime: &PgRoleName,
) -> Result<(), RoomObservationSchemaError> {
    validate_identifier(runtime.as_str(), "room observation runtime role")
        .map_err(|_| RoomObservationSchemaError::UnsafeRuntime)?;
    verify_runtime_role(tx, identity, runtime.as_str()).await?;
    let schema = quote_identifier(&identity.schema.name.0);
    tx.execute(
        &format!(
            "GRANT USAGE ON SCHEMA {schema} TO \"{}\", pg_monitor",
            runtime.as_str()
        ),
        (),
    )
    .await?;
    let ledger = qualified_table(&identity.schema.name.0, "_migrations");
    tx.execute(
        &format!("GRANT SELECT ON TABLE {ledger} TO \"{}\"", runtime.as_str()),
        (),
    )
    .await?;
    for table in std::iter::once(&OWNER_STATE_CONTRACT).chain(DATA_TABLES.iter()) {
        let qualified = qualified_table(&identity.schema.name.0, table.name);
        let privileges = if table.read_only {
            "SELECT"
        } else {
            "SELECT, INSERT, UPDATE, DELETE"
        };
        tx.execute(
            &format!(
                "GRANT {privileges} ON TABLE {qualified} TO \"{}\"",
                runtime.as_str()
            ),
            (),
        )
        .await?;
    }
    Ok(())
}

async fn verify_attested_lineage(
    tx: &mut Transaction<'_>,
    capability: &AttestedPostgresWriterCapability,
    expected: &AttestedLineage,
    identity: &PgIdentity,
) -> Result<(), RoomObservationSchemaError> {
    let config = LineageConfig {
        deployment_uuid: Some(expected.deployment_uuid),
        action: None,
    };
    let table = format!("{}.\"_lineage\"", quote_identifier(&identity.schema.name.0));
    let observed = crate::db::lineage::verify_qualified_with_attested_identity_in_transaction(
        tx,
        &config,
        &table,
        capability.identity(),
    )
    .await
    .map_err(|_| RoomObservationSchemaError::IdentityMismatch)?;
    if &observed != expected {
        return Err(RoomObservationSchemaError::IdentityMismatch);
    }
    Ok(())
}

async fn verify_active_schema(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
) -> Result<(), RoomObservationSchemaError> {
    let mut rows = tx
        .query(
            "SELECT pg_catalog.current_schema()::text, namespace.oid::bigint \
             FROM pg_catalog.pg_namespace namespace WHERE namespace.nspname = ?",
            crate::db_params![&identity.schema.name.0],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(RoomObservationSchemaError::IdentityMismatch);
    };
    if row.get::<Option<String>>(0)?.as_deref() != Some(identity.schema.name.0.as_str())
        || row.get::<i64>(1)? != i64::from(identity.schema.oid)
    {
        return Err(RoomObservationSchemaError::IdentityMismatch);
    }
    Ok(())
}

async fn runtime_login_from_transaction(
    tx: &mut Transaction<'_>,
) -> Result<String, RoomObservationSchemaError> {
    let mut rows = tx
        .query(
            "SELECT session_user::text, current_user::text, \
             session_user::text = current_user::text",
            (),
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    };
    let session_user: String = row.get(0)?;
    if !row.get::<bool>(2)? {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    }
    Ok(session_user)
}

async fn verify_runtime_role(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
    runtime: &str,
) -> Result<(), RoomObservationSchemaError> {
    let mut rows = tx
        .query(
            "SELECT role.rolcanlogin AND NOT role.rolsuper AND NOT role.rolcreaterole \
               AND NOT role.rolcreatedb AND NOT role.rolreplication AND NOT role.rolbypassrls \
               AND role.oid <> database.datdba AND role.oid <> namespace.nspowner \
               AND NOT pg_catalog.has_database_privilege(role.oid, database.oid, 'CREATE,TEMP') \
               AND NOT pg_catalog.has_schema_privilege(role.oid, namespace.oid, 'CREATE') \
               AND NOT pg_catalog.pg_has_role(role.oid, namespace.nspowner, 'SET') \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership \
                               WHERE membership.member = role.oid OR membership.roleid = role.oid) \
             FROM pg_catalog.pg_roles role \
             JOIN pg_catalog.pg_namespace namespace ON namespace.nspname = ? \
             JOIN pg_catalog.pg_database database \
               ON database.datname = pg_catalog.current_database() \
             WHERE role.rolname = ?",
            crate::db_params![&identity.schema.name.0, runtime],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    };
    if !row.get::<bool>(0)? {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    }
    Ok(())
}

struct ObservationVerification<'a> {
    attestation_definer: &'a str,
    runtime: &'a str,
    marker_revision: &'a str,
    profile: Option<&'a crate::pubsub::RestrictedServingProfileV1>,
}

async fn verify_room_observation_schema_shape(
    tx: &mut Transaction<'_>,
    expected: &AttestedLineage,
    identity: &PgIdentity,
    designated_migrator: &str,
    verification: ObservationVerification<'_>,
) -> Result<(), RoomObservationSchemaError> {
    let ObservationVerification {
        attestation_definer,
        runtime,
        marker_revision,
        profile,
    } = verification;
    verify_runtime_role(tx, identity, runtime).await?;
    verify_schema_owner_role(tx, identity, designated_migrator).await?;
    if let Some(profile) = profile {
        crate::pubsub::runtime_attach::verify_restricted_v1_graph_tx(
            tx,
            identity,
            attestation_definer,
            profile,
        )
        .await
        .map_err(|_| RoomObservationSchemaError::UnsafeRuntime)?;
        let tables: Vec<_> = std::iter::once(OWNER_STATE_CONTRACT.name)
            .chain(DATA_TABLES.iter().map(|table| table.name))
            .collect();
        crate::pubsub::runtime_attach::verify_no_definer_writers_tx(
            tx,
            &identity.schema.name.0,
            &tables,
        )
        .await
        .map_err(|_| RoomObservationSchemaError::UnsafeRuntime)?;
    }
    verify_schema_acl(tx, identity, runtime, attestation_definer, profile).await?;
    verify_no_runtime_callable_owner_definers(tx, identity, runtime).await?;
    verify_no_runtime_view_bypasses(tx, identity, runtime).await?;
    verify_no_runtime_definer_triggers(tx, identity, runtime).await?;
    verify_migration_ledger_runtime_acl(tx, identity, runtime).await?;
    verify_owner_marker(
        tx,
        expected,
        identity,
        designated_migrator,
        Some(marker_revision),
    )
    .await?;
    verify_table_shape(tx, &identity.schema.name.0, &OWNER_STATE_CONTRACT).await?;
    verify_runtime_acl(tx, identity, &OWNER_STATE_CONTRACT, runtime).await?;
    verify_pg_monitor_grants(tx, identity).await?;
    for table in DATA_TABLES {
        verify_table_shape(tx, &identity.schema.name.0, table).await?;
        verify_runtime_acl(tx, identity, table, runtime).await?;
    }
    Ok(())
}

async fn verify_schema_owner_role(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
    designated_migrator: &str,
) -> Result<(), RoomObservationSchemaError> {
    let mut rows = tx
        .query(
            "SELECT NOT owner.rolcanlogin AND NOT owner.rolsuper \
               AND NOT owner.rolcreatedb AND NOT owner.rolcreaterole \
               AND NOT owner.rolreplication AND NOT owner.rolbypassrls \
               AND owner.oid <> database.datdba \
               AND NOT pg_catalog.has_database_privilege(owner.oid, database.oid, 'CREATE,TEMP') \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership \
                               WHERE membership.member = owner.oid) \
               AND (SELECT pg_catalog.count(*) FROM pg_catalog.pg_auth_members membership \
                    WHERE membership.roleid = owner.oid) = 1 \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership \
                               JOIN pg_catalog.pg_roles member_role \
                                 ON member_role.oid = membership.member \
                               WHERE membership.roleid = owner.oid \
                                 AND (member_role.rolname <> ? OR NOT member_role.rolcanlogin \
                                   OR member_role.rolsuper OR member_role.rolcreatedb \
                                   OR member_role.rolcreaterole OR member_role.rolreplication \
                                   OR member_role.rolbypassrls OR member_role.oid = database.datdba \
                                   OR pg_catalog.has_database_privilege( \
                                     member_role.oid, database.oid, 'CREATE,TEMP') \
                                   OR pg_catalog.has_schema_privilege( \
                                     member_role.oid, namespace.oid, 'CREATE') \
                                   OR NOT membership.set_option OR membership.inherit_option \
                                   OR membership.admin_option \
                                   OR EXISTS (SELECT 1 \
                                              FROM pg_catalog.pg_auth_members inbound \
                                              WHERE inbound.roleid = member_role.oid) \
                                   OR NOT pg_catalog.pg_has_role( \
                                     member_role.oid, owner.oid, 'SET') \
                                   OR pg_catalog.pg_has_role( \
                                     member_role.oid, owner.oid, 'USAGE') \
                                   OR (SELECT pg_catalog.count(*) \
                                       FROM pg_catalog.pg_auth_members member_of \
                                       WHERE member_of.member = member_role.oid) <> 1)) \
             FROM pg_catalog.pg_namespace namespace \
             JOIN pg_catalog.pg_roles owner ON owner.oid = namespace.nspowner \
             JOIN pg_catalog.pg_database database \
               ON database.datname = pg_catalog.current_database() \
             WHERE namespace.nspname = ?",
            crate::db_params![designated_migrator, &identity.schema.name.0],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(RoomObservationSchemaError::UnsafeOwner);
    };
    if !row.get::<bool>(0)? {
        return Err(RoomObservationSchemaError::UnsafeOwner);
    }
    Ok(())
}

async fn verify_table_shape(
    tx: &mut Transaction<'_>,
    schema: &str,
    contract: &TableContract,
) -> Result<(), RoomObservationSchemaError> {
    let mut rows = tx
        .query(
            "SELECT relation.relkind = 'r' AND relation.relpersistence = 'p' \
               AND NOT relation.relispartition AND relation.reloftype = 0 \
               AND NOT relation.relrowsecurity AND NOT relation.relforcerowsecurity \
               AND relation.relowner = namespace.nspowner \
               AND relation.reloptions IS NULL \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_rewrite rewrite \
                               WHERE rewrite.ev_class = relation.oid) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_policy policy \
                               WHERE policy.polrelid = relation.oid) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_inherits inheritance \
                               WHERE inheritance.inhrelid = relation.oid \
                                  OR inheritance.inhparent = relation.oid) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger trigger_ \
                               WHERE trigger_.tgrelid = relation.oid) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_constraint inbound \
                               WHERE inbound.contype = 'f' AND inbound.confrelid = relation.oid) \
             FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             WHERE namespace.nspname = ? AND relation.relname = ?",
            crate::db_params![schema, contract.name],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(RoomObservationSchemaError::InvalidSchemaShape);
    };
    if !row.get::<bool>(0)? {
        return Err(RoomObservationSchemaError::InvalidSchemaShape);
    }
    verify_columns(tx, schema, contract).await?;
    verify_constraints(tx, schema, contract).await?;
    verify_indexes(tx, schema, contract).await?;
    Ok(())
}

async fn verify_columns(
    tx: &mut Transaction<'_>,
    schema: &str,
    contract: &TableContract,
) -> Result<(), RoomObservationSchemaError> {
    let mut rows = tx
        .query(
            "SELECT attribute.attname::text, \
                    pg_catalog.format_type(attribute.atttypid, attribute.atttypmod), \
                    attribute.attnotnull, attribute.attcollation = type_.typcollation, \
                    attribute.attgenerated::text, attribute.attidentity::text, \
                    pg_catalog.pg_get_expr(default_.adbin, default_.adrelid) \
             FROM pg_catalog.pg_attribute attribute \
             JOIN pg_catalog.pg_class relation ON relation.oid = attribute.attrelid \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             JOIN pg_catalog.pg_type type_ ON type_.oid = attribute.atttypid \
             LEFT JOIN pg_catalog.pg_attrdef default_ \
               ON default_.adrelid = attribute.attrelid AND default_.adnum = attribute.attnum \
             WHERE namespace.nspname = ? AND relation.relname = ? AND relation.relkind = 'r' \
               AND attribute.attnum > 0 AND NOT attribute.attisdropped",
            crate::db_params![schema, contract.name],
        )
        .await?;
    let mut actual = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        actual.insert(
            row.get::<String>(0)?,
            (
                row.get::<String>(1)?,
                row.get::<bool>(2)?,
                row.get::<bool>(3)?,
                row.get::<String>(4)?,
                row.get::<String>(5)?,
                row.get::<Option<String>>(6)?,
            ),
        );
    }
    if actual.len() != contract.columns.len() {
        return Err(RoomObservationSchemaError::InvalidSchemaShape);
    }
    for column in contract.columns {
        let Some((kind, not_null, collation_matches, generated, identity, default)) =
            actual.get(column.name)
        else {
            return Err(RoomObservationSchemaError::InvalidSchemaShape);
        };
        let default_matches = match column.default {
            ColumnDefault::None => default.is_none(),
            ColumnDefault::Zero => default.as_deref() == Some("0"),
        };
        if kind != column.kind
            || *not_null != column.not_null
            || !collation_matches
            || !generated.is_empty()
            || !identity.is_empty()
            || !default_matches
        {
            return Err(RoomObservationSchemaError::InvalidSchemaShape);
        }
    }
    Ok(())
}

#[derive(Default)]
struct ActualConstraint {
    kind: String,
    deferrable: bool,
    deferred: bool,
    validated: bool,
    definition: String,
    columns: Vec<String>,
}

async fn verify_constraints(
    tx: &mut Transaction<'_>,
    schema: &str,
    contract: &TableContract,
) -> Result<(), RoomObservationSchemaError> {
    let mut rows = tx
        .query(
            "SELECT constraint_.conname::text, constraint_.contype::text, \
                    constraint_.condeferrable, constraint_.condeferred, \
                    constraint_.convalidated, pg_catalog.pg_get_constraintdef(constraint_.oid), \
                    attribute.attname::text, key.position::bigint \
             FROM pg_catalog.pg_constraint constraint_ \
             JOIN pg_catalog.pg_class relation ON relation.oid = constraint_.conrelid \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             LEFT JOIN LATERAL pg_catalog.unnest(constraint_.conkey) \
               WITH ORDINALITY key(attnum, position) ON true \
             LEFT JOIN pg_catalog.pg_attribute attribute \
               ON attribute.attrelid = relation.oid AND attribute.attnum = key.attnum \
             WHERE namespace.nspname = ? AND relation.relname = ? \
             ORDER BY constraint_.conname, key.position",
            crate::db_params![schema, contract.name],
        )
        .await?;
    let mut actual: BTreeMap<String, ActualConstraint> = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let name: String = row.get(0)?;
        let constraint = actual.entry(name).or_default();
        constraint.kind = row.get(1)?;
        constraint.deferrable = row.get(2)?;
        constraint.deferred = row.get(3)?;
        constraint.validated = row.get(4)?;
        constraint.definition = row.get(5)?;
        if let Some(column) = row.get::<Option<String>>(6)? {
            constraint.columns.push(column);
        }
    }
    if actual.len() != contract.constraints.len() {
        return Err(RoomObservationSchemaError::InvalidSchemaShape);
    }
    let mut matched = BTreeSet::new();
    for expected in contract.constraints {
        let Some((name, constraint)) = actual.iter().find(|(name, constraint)| {
            !matched.contains(*name)
                && constraint.kind == expected.kind
                && constraint
                    .columns
                    .iter()
                    .map(String::as_str)
                    .eq(expected.columns.iter().copied())
                && expected
                    .name
                    .is_none_or(|expected_name| name.as_str() == expected_name)
        }) else {
            return Err(RoomObservationSchemaError::InvalidSchemaShape);
        };
        if constraint.deferrable || constraint.deferred || !constraint.validated {
            return Err(RoomObservationSchemaError::InvalidSchemaShape);
        }
        if let Some(expected_check) = expected.normalized_check {
            if normalize_check_definition(&constraint.definition) != expected_check {
                return Err(RoomObservationSchemaError::InvalidSchemaShape);
            }
        }
        matched.insert(name.clone());
    }
    Ok(())
}

fn normalize_check_definition(definition: &str) -> String {
    definition
        .to_ascii_lowercase()
        .replace("::text[]", "")
        .replace("::text", "")
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '(' && *character != ')')
        .collect::<String>()
        .trim_start_matches("check")
        .to_owned()
}

#[derive(Default)]
struct ActualIndex {
    primary: bool,
    unique: bool,
    valid: bool,
    ready: bool,
    live: bool,
    immediate: bool,
    key_count: i64,
    attribute_count: i64,
    method: String,
    predicate_free: bool,
    expression_free: bool,
    constraint_backed: bool,
    definition: String,
    predicate: Option<String>,
    columns: Vec<String>,
}

async fn verify_indexes(
    tx: &mut Transaction<'_>,
    schema: &str,
    contract: &TableContract,
) -> Result<(), RoomObservationSchemaError> {
    let mut rows = tx
        .query(
            "SELECT index_relation.relname::text, index_.indisprimary, index_.indisunique, \
                    index_.indisvalid, index_.indisready, index_.indislive, \
                    index_.indimmediate, index_.indnatts::bigint, \
                    index_.indnkeyatts::bigint, access_method.amname::text, \
                    index_.indpred IS NULL, index_.indexprs IS NULL, \
                    EXISTS (SELECT 1 FROM pg_catalog.pg_constraint constraint_ \
                            WHERE constraint_.conindid = index_.indexrelid), \
                    pg_catalog.pg_get_indexdef(index_.indexrelid), \
                    attribute.attname::text, key.position::bigint, \
                    pg_catalog.pg_get_expr(index_.indpred, index_.indrelid) \
             FROM pg_catalog.pg_index index_ \
             JOIN pg_catalog.pg_class relation ON relation.oid = index_.indrelid \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             JOIN pg_catalog.pg_class index_relation ON index_relation.oid = index_.indexrelid \
             JOIN pg_catalog.pg_am access_method ON access_method.oid = index_relation.relam \
             LEFT JOIN LATERAL pg_catalog.unnest(index_.indkey) \
               WITH ORDINALITY key(attnum, position) ON true \
             LEFT JOIN pg_catalog.pg_attribute attribute \
               ON attribute.attrelid = relation.oid AND attribute.attnum = key.attnum \
             WHERE namespace.nspname = ? AND relation.relname = ? \
             ORDER BY index_relation.relname, key.position",
            crate::db_params![schema, contract.name],
        )
        .await?;
    let mut actual: BTreeMap<String, ActualIndex> = BTreeMap::new();
    while let Some(row) = rows.next().await? {
        let name: String = row.get(0)?;
        let index = actual.entry(name).or_default();
        index.primary = row.get(1)?;
        index.unique = row.get(2)?;
        index.valid = row.get(3)?;
        index.ready = row.get(4)?;
        index.live = row.get(5)?;
        index.immediate = row.get(6)?;
        index.attribute_count = row.get(7)?;
        index.key_count = row.get(8)?;
        index.method = row.get(9)?;
        index.predicate_free = row.get(10)?;
        index.expression_free = row.get(11)?;
        index.constraint_backed = row.get(12)?;
        index.definition = row.get(13)?;
        index.predicate = row.get(16)?;
        index
            .columns
            .push(row.get::<Option<String>>(14)?.unwrap_or_default());
    }
    if actual.len() != contract.indexes.len() {
        return Err(RoomObservationSchemaError::InvalidSchemaShape);
    }
    let mut matched = BTreeSet::new();
    for expected in contract.indexes {
        let Some((name, index)) = actual.iter().find(|(name, index)| {
            !matched.contains(*name)
                && expected
                    .name
                    .is_none_or(|expected_name| name.as_str() == expected_name)
                && index.primary == expected.primary
                && index.unique == expected.unique
                && index
                    .columns
                    .iter()
                    .map(String::as_str)
                    .eq(expected.columns.iter().copied())
        }) else {
            return Err(RoomObservationSchemaError::InvalidSchemaShape);
        };
        let expected_suffix = format!("btree({})", expected.columns.join(","));
        let index_definition = index
            .definition
            .to_ascii_lowercase()
            .split_once(" using ")
            .map(|(_, suffix)| {
                suffix
                    .split_once(" where ")
                    .map_or(suffix, |(definition, _)| definition)
                    .chars()
                    .filter(|character| !character.is_whitespace())
                    .collect::<String>()
            });
        let predicate_matches = match (expected.predicate, index.predicate.as_deref()) {
            (None, None) => true,
            (Some(expected), Some(actual)) => normalize_index_predicate(actual) == expected,
            _ => false,
        };
        if !index.valid
            || !index.ready
            || !index.live
            || !index.immediate
            || index.key_count != expected.columns.len() as i64
            || index.attribute_count != expected.columns.len() as i64
            || index.method != "btree"
            || index.predicate_free != expected.predicate.is_none()
            || !index.expression_free
            || index.constraint_backed != (expected.unique || expected.primary)
            || index_definition.as_deref() != Some(expected_suffix.as_str())
            || !predicate_matches
        {
            return Err(RoomObservationSchemaError::InvalidSchemaShape);
        }
        matched.insert(name.clone());
    }
    Ok(())
}

fn normalize_index_predicate(predicate: &str) -> String {
    predicate
        .to_ascii_lowercase()
        .replace("::character varying[]", "")
        .replace("::varchar[]", "")
        .replace("::text[]", "")
        .replace("::character varying", "")
        .replace("::varchar", "")
        .replace("::text", "")
        .chars()
        .filter(|character| !character.is_whitespace() && *character != '(' && *character != ')')
        .collect()
}

async fn verify_schema_acl(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
    runtime: &str,
    attestation_definer: &str,
    profile: Option<&crate::pubsub::RestrictedServingProfileV1>,
) -> Result<(), RoomObservationSchemaError> {
    let extra_roles = profile.map_or_else(String::new, |profile| {
        format!(
            ", {}::pg_catalog.oid, {}::pg_catalog.oid",
            profile.api_oid, profile.helper_oid
        )
    });
    let query = format!(
            "SELECT pg_catalog.has_schema_privilege(runtime.oid, namespace.oid, 'USAGE') \
               AND NOT pg_catalog.has_schema_privilege(runtime.oid, namespace.oid, 'CREATE') \
               AND pg_catalog.has_schema_privilege(definer.oid, namespace.oid, 'USAGE') \
               AND NOT pg_catalog.has_schema_privilege(definer.oid, namespace.oid, 'CREATE') \
               AND pg_catalog.has_table_privilege(runtime.oid, lineage.oid, 'SELECT') \
               AND NOT pg_catalog.has_table_privilege( \
                 runtime.oid, lineage.oid, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') \
               AND NOT pg_catalog.has_any_column_privilege(runtime.oid, lineage.oid, 'INSERT,UPDATE') \
               AND pg_catalog.has_table_privilege(definer.oid, lineage.oid, 'SELECT') \
               AND NOT pg_catalog.has_table_privilege( \
                 definer.oid, lineage.oid, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') \
               AND NOT pg_catalog.has_any_column_privilege(definer.oid, lineage.oid, 'INSERT,UPDATE') \
               AND pg_catalog.has_schema_privilege(monitor.oid, namespace.oid, 'USAGE') \
               AND NOT pg_catalog.has_schema_privilege(monitor.oid, namespace.oid, 'CREATE') \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                 COALESCE(namespace.nspacl, pg_catalog.acldefault('n', namespace.nspowner))) acl \
                 WHERE acl.grantee NOT IN (namespace.nspowner, runtime.oid, definer.oid, monitor.oid{extra_roles}) \
                    OR (acl.grantee IN (runtime.oid, definer.oid, monitor.oid{extra_roles}) \
                      AND (acl.grantor <> namespace.nspowner OR acl.is_grantable \
                        OR acl.privilege_type <> 'USAGE'))) \
               AND (SELECT pg_catalog.count(*) FROM pg_catalog.aclexplode( \
                 COALESCE(namespace.nspacl, pg_catalog.acldefault('n', namespace.nspowner))) acl \
                 WHERE acl.grantee = runtime.oid) = 1 \
               AND (SELECT pg_catalog.count(*) FROM pg_catalog.aclexplode( \
                 COALESCE(namespace.nspacl, pg_catalog.acldefault('n', namespace.nspowner))) acl \
                 WHERE acl.grantee = monitor.oid) = 1 \
               AND (SELECT pg_catalog.count(*) FROM pg_catalog.aclexplode( \
                 COALESCE(namespace.nspacl, pg_catalog.acldefault('n', namespace.nspowner))) acl \
                 WHERE acl.grantee = definer.oid) = 1 \
             FROM pg_catalog.pg_namespace namespace \
             JOIN pg_catalog.pg_class lineage \
               ON lineage.relnamespace = namespace.oid AND lineage.relname = '_lineage' \
              AND lineage.relkind = 'r' \
             JOIN pg_catalog.pg_roles runtime ON runtime.rolname = ? \
             JOIN pg_catalog.pg_roles definer ON definer.rolname = ? \
             JOIN pg_catalog.pg_roles monitor ON monitor.rolname = 'pg_monitor' \
             WHERE namespace.nspname = ?"
    );
    let mut rows = tx
        .query(
            &query,
            crate::db_params![runtime, attestation_definer, &identity.schema.name.0],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    };
    if !row.get::<bool>(0)? {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    }
    Ok(())
}

async fn verify_no_runtime_callable_owner_definers(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
    runtime: &str,
) -> Result<(), RoomObservationSchemaError> {
    // The designated owner may create functions outside the target schema,
    // and a view can invoke them without namespace USAGE. Check every
    // owner-owned callable SECURITY DEFINER routine in this database.
    let mut rows = tx
        .query(
            "SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc routine \
               JOIN pg_catalog.pg_namespace owner_namespace \
                 ON owner_namespace.nspname = ? \
                AND routine.proowner = owner_namespace.nspowner \
               JOIN pg_catalog.pg_roles runtime ON runtime.rolname = ? \
               WHERE routine.prosecdef \
                 AND routine.prorettype NOT IN ( \
                   'pg_catalog.trigger'::regtype, 'pg_catalog.event_trigger'::regtype) \
                 AND pg_catalog.has_function_privilege( \
                   runtime.oid, routine.oid, 'EXECUTE'))",
            crate::db_params![&identity.schema.name.0, runtime],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    };
    if !row.get::<bool>(0)? {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    }
    Ok(())
}

async fn verify_no_runtime_view_bypasses(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
    runtime: &str,
) -> Result<(), RoomObservationSchemaError> {
    // Owner-rights views inherit their owner's relation privileges. Inspect
    // views in every runtime-usable schema, and keep unrelated views valid when
    // each effective operation is also granted on target-schema dependencies.
    let mut rows = tx
        .query(
            "WITH RECURSIVE view_dependencies(root_oid, referenced_oid) AS ( \
               SELECT root.oid, dependency.refobjid \
                 FROM pg_catalog.pg_class root \
                 JOIN pg_catalog.pg_namespace root_namespace \
                   ON root_namespace.oid = root.relnamespace \
                 JOIN pg_catalog.pg_rewrite rewrite ON rewrite.ev_class = root.oid \
                 JOIN pg_catalog.pg_depend dependency \
                   ON dependency.classid = 'pg_catalog.pg_rewrite'::regclass \
                  AND dependency.objid = rewrite.oid \
                  AND dependency.refclassid = 'pg_catalog.pg_class'::regclass \
                WHERE root.relkind IN ('v','m') \
               UNION \
               SELECT view_dependencies.root_oid, dependency.refobjid \
                 FROM view_dependencies \
                 JOIN pg_catalog.pg_class nested_view \
                   ON nested_view.oid = view_dependencies.referenced_oid \
                  AND nested_view.relkind IN ('v','m') \
                 JOIN pg_catalog.pg_rewrite rewrite \
                   ON rewrite.ev_class = nested_view.oid \
                 JOIN pg_catalog.pg_depend dependency \
                   ON dependency.classid = 'pg_catalog.pg_rewrite'::regclass \
                  AND dependency.objid = rewrite.oid \
                  AND dependency.refclassid = 'pg_catalog.pg_class'::regclass \
             ) \
             SELECT NOT EXISTS ( \
               SELECT 1 FROM pg_catalog.pg_class view_ \
               JOIN pg_catalog.pg_namespace namespace \
                 ON namespace.oid = view_.relnamespace \
               JOIN pg_catalog.pg_namespace target_namespace \
                 ON target_namespace.nspname = ? \
               JOIN pg_catalog.pg_roles runtime ON runtime.rolname = ? \
               WHERE view_.relkind IN ('v','m') \
                 AND (pg_catalog.has_table_privilege(runtime.oid, view_.oid, 'SELECT') \
                   OR pg_catalog.has_table_privilege(runtime.oid, view_.oid, 'INSERT') \
                   OR pg_catalog.has_table_privilege(runtime.oid, view_.oid, 'UPDATE') \
                   OR pg_catalog.has_table_privilege(runtime.oid, view_.oid, 'DELETE') \
                   OR EXISTS (SELECT 1 FROM pg_catalog.pg_attribute view_attribute \
                     CROSS JOIN LATERAL pg_catalog.aclexplode(view_attribute.attacl) column_acl \
                     WHERE view_attribute.attrelid = view_.oid \
                       AND view_attribute.attnum > 0 AND NOT view_attribute.attisdropped \
                       AND column_acl.grantee IN (0, runtime.oid) \
                       AND column_acl.privilege_type IN ('SELECT','INSERT','UPDATE','DELETE'))) \
                 AND (view_.relkind = 'm' OR NOT COALESCE( \
                   'security_invoker=true' = ANY(view_.reloptions), false)) \
                 AND EXISTS ( \
                   SELECT 1 FROM view_dependencies dependency \
                   JOIN pg_catalog.pg_class referenced \
                     ON referenced.oid = dependency.referenced_oid \
                   JOIN pg_catalog.pg_namespace referenced_namespace \
                     ON referenced_namespace.oid = referenced.relnamespace \
                   WHERE dependency.root_oid = view_.oid \
                     AND referenced_namespace.oid = target_namespace.oid \
                     AND ((pg_catalog.has_table_privilege( \
                             runtime.oid, view_.oid, 'SELECT') \
                           AND NOT pg_catalog.has_table_privilege( \
                             runtime.oid, referenced.oid, 'SELECT')) \
                       OR (pg_catalog.has_table_privilege( \
                             runtime.oid, view_.oid, 'INSERT') \
                           AND NOT pg_catalog.has_table_privilege( \
                             runtime.oid, referenced.oid, 'INSERT')) \
                       OR (pg_catalog.has_table_privilege( \
                             runtime.oid, view_.oid, 'UPDATE') \
                           AND NOT pg_catalog.has_table_privilege( \
                             runtime.oid, referenced.oid, 'UPDATE')) \
                       OR (pg_catalog.has_table_privilege( \
                             runtime.oid, view_.oid, 'DELETE') \
                           AND NOT pg_catalog.has_table_privilege( \
                             runtime.oid, referenced.oid, 'DELETE')) \
                       OR EXISTS (SELECT 1 FROM pg_catalog.pg_attribute view_attribute \
                         CROSS JOIN LATERAL pg_catalog.aclexplode(view_attribute.attacl) column_acl \
                         WHERE view_attribute.attrelid = view_.oid \
                           AND view_attribute.attnum > 0 AND NOT view_attribute.attisdropped \
                           AND column_acl.grantee IN (0, runtime.oid) \
                           AND column_acl.privilege_type IN ('SELECT','INSERT','UPDATE','DELETE'))))) ",
            crate::db_params![&identity.schema.name.0, runtime],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(RoomObservationSchemaError::InvalidSchemaShape);
    };
    if !row.get::<bool>(0)? {
        return Err(RoomObservationSchemaError::InvalidSchemaShape);
    }
    Ok(())
}

async fn verify_no_runtime_definer_triggers(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
    runtime: &str,
) -> Result<(), RoomObservationSchemaError> {
    // Trigger functions cannot be called directly, but a SECURITY DEFINER
    // trigger attached to a runtime-writable relation can still perform DML
    // with its owner's privileges.
    let mut rows = tx
        .query(
            "WITH RECURSIVE trigger_relations(trigger_oid, relation_oid) AS ( \
               SELECT trigger_.oid, trigger_.tgrelid \
                 FROM pg_catalog.pg_trigger trigger_ \
                WHERE NOT trigger_.tgisinternal \
               UNION \
               SELECT trigger_relations.trigger_oid, inheritance.inhparent \
                 FROM trigger_relations \
                 JOIN pg_catalog.pg_inherits inheritance \
                   ON inheritance.inhrelid = trigger_relations.relation_oid \
             ) \
             SELECT NOT EXISTS (SELECT 1 FROM pg_catalog.pg_trigger trigger_ \
               JOIN trigger_relations attached \
                 ON attached.trigger_oid = trigger_.oid \
               JOIN pg_catalog.pg_class relation ON relation.oid = trigger_.tgrelid \
               JOIN pg_catalog.pg_namespace relation_namespace \
                 ON relation_namespace.oid = relation.relnamespace \
               JOIN pg_catalog.pg_class privilege_relation \
                 ON privilege_relation.oid = attached.relation_oid \
               JOIN pg_catalog.pg_namespace owner_namespace \
                 ON owner_namespace.nspname = ? \
               JOIN pg_catalog.pg_proc routine ON routine.oid = trigger_.tgfoid \
               JOIN pg_catalog.pg_roles runtime ON runtime.rolname = ? \
               WHERE NOT trigger_.tgisinternal AND routine.prosecdef \
                 AND (relation_namespace.oid = owner_namespace.oid \
                   OR routine.proowner = owner_namespace.nspowner) \
                 AND (((trigger_.tgtype & 4) <> 0 \
                       AND (pg_catalog.has_table_privilege( \
                              runtime.oid, privilege_relation.oid, 'INSERT') \
                         OR pg_catalog.has_any_column_privilege( \
                              runtime.oid, privilege_relation.oid, 'INSERT'))) \
                   OR ((trigger_.tgtype & 8) <> 0 \
                       AND pg_catalog.has_table_privilege( \
                         runtime.oid, privilege_relation.oid, 'DELETE')) \
                   OR ((trigger_.tgtype & 16) <> 0 \
                       AND (pg_catalog.has_table_privilege( \
                              runtime.oid, privilege_relation.oid, 'UPDATE') \
                         OR pg_catalog.has_any_column_privilege( \
                              runtime.oid, privilege_relation.oid, 'UPDATE'))) \
                   OR ((trigger_.tgtype & 32) <> 0 \
                       AND pg_catalog.has_table_privilege( \
                         runtime.oid, privilege_relation.oid, 'TRUNCATE'))))",
            crate::db_params![&identity.schema.name.0, runtime],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    };
    if !row.get::<bool>(0)? {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    }
    Ok(())
}

async fn verify_migration_ledger_runtime_acl(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
    runtime: &str,
) -> Result<(), RoomObservationSchemaError> {
    let mut rows = tx
        .query(
            "SELECT relation.relkind = 'r' AND relation.relowner = namespace.nspowner \
               AND runtime.oid <> relation.relowner \
               AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'SELECT') \
               AND NOT pg_catalog.has_table_privilege( \
                 runtime.oid, relation.oid, \
                 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER') \
               AND NOT pg_catalog.has_table_privilege( \
                 runtime.oid, relation.oid, \
                 'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION, \
                  DELETE WITH GRANT OPTION,TRUNCATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION, \
                  TRIGGER WITH GRANT OPTION') \
               AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000 \
                 THEN NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'MAINTAIN') \
                 ELSE true END \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                 COALESCE(relation.relacl, pg_catalog.acldefault('r', relation.relowner))) acl \
                 WHERE acl.grantee = 0 OR acl.grantee NOT IN (relation.relowner, runtime.oid) \
                    OR (acl.grantee = runtime.oid AND (acl.grantor <> relation.relowner \
                      OR acl.is_grantable OR acl.privilege_type <> 'SELECT'))) \
               AND (SELECT pg_catalog.count(*) FROM pg_catalog.aclexplode( \
                 COALESCE(relation.relacl, pg_catalog.acldefault('r', relation.relowner))) acl \
                 WHERE acl.grantee = runtime.oid) = 1 \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute attribute \
                 WHERE attribute.attrelid = relation.oid AND attribute.attnum > 0 \
                   AND NOT attribute.attisdropped AND attribute.attacl IS NOT NULL) \
             FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             JOIN pg_catalog.pg_roles runtime ON runtime.rolname = ? \
             WHERE namespace.nspname = ? AND relation.relname = '_migrations'",
            crate::db_params![runtime, &identity.schema.name.0],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    };
    if !row.get::<bool>(0)? {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    }
    Ok(())
}

async fn verify_runtime_acl(
    tx: &mut Transaction<'_>,
    identity: &PgIdentity,
    contract: &TableContract,
    runtime: &str,
) -> Result<(), RoomObservationSchemaError> {
    let expected_count = if contract.read_only { 1 } else { 4 };
    let expected_monitor_count = if contract.read_only { 0 } else { 1 };
    let table = contract.name;
    let mut rows = tx
        .query(
            "SELECT relation.relkind = 'r' AND relation.relowner = namespace.nspowner \
               AND runtime.oid <> relation.relowner AND monitor.oid <> relation.relowner \
               AND pg_catalog.has_schema_privilege(runtime.oid, namespace.oid, 'USAGE') \
               AND NOT pg_catalog.has_schema_privilege(runtime.oid, namespace.oid, 'CREATE') \
               AND pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'SELECT') \
               AND (? = 1 OR pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'INSERT')) \
               AND (? = 1 OR pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'UPDATE')) \
               AND (? = 1 OR pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'DELETE')) \
               AND (? = 0 OR NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'INSERT,UPDATE,DELETE')) \
               AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'TRUNCATE,REFERENCES,TRIGGER') \
               AND NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, \
                 'SELECT WITH GRANT OPTION,INSERT WITH GRANT OPTION,UPDATE WITH GRANT OPTION,DELETE WITH GRANT OPTION,TRUNCATE WITH GRANT OPTION,REFERENCES WITH GRANT OPTION,TRIGGER WITH GRANT OPTION') \
               AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000 \
                 THEN NOT pg_catalog.has_table_privilege(runtime.oid, relation.oid, 'MAINTAIN') ELSE true END \
               AND (? = 1 OR pg_catalog.has_table_privilege(monitor.oid, relation.oid, 'SELECT')) \
               AND (? = 0 OR NOT pg_catalog.has_table_privilege( \
                 monitor.oid, relation.oid, 'INSERT,UPDATE,DELETE,TRUNCATE,REFERENCES,TRIGGER')) \
               AND CASE WHEN pg_catalog.current_setting('server_version_num')::integer >= 170000 \
                 THEN NOT pg_catalog.has_table_privilege(monitor.oid, relation.oid, 'MAINTAIN') ELSE true END \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.aclexplode( \
                 COALESCE(relation.relacl, pg_catalog.acldefault('r', relation.relowner))) acl \
                 WHERE acl.grantee = 0 \
                    OR acl.grantee NOT IN (relation.relowner, runtime.oid, monitor.oid) \
                    OR (acl.grantee = runtime.oid AND (acl.grantor <> relation.relowner OR acl.is_grantable \
                      OR acl.privilege_type NOT IN ('SELECT','INSERT','UPDATE','DELETE'))) \
                    OR (acl.grantee = monitor.oid AND (acl.grantor <> relation.relowner OR acl.is_grantable \
                      OR acl.privilege_type <> 'SELECT'))) \
               AND (SELECT pg_catalog.count(*) FROM pg_catalog.aclexplode( \
                 COALESCE(relation.relacl, pg_catalog.acldefault('r', relation.relowner))) acl \
                 WHERE acl.grantee = runtime.oid) = ? \
               AND (SELECT pg_catalog.count(*) FROM pg_catalog.aclexplode( \
                 COALESCE(relation.relacl, pg_catalog.acldefault('r', relation.relowner))) acl \
                 WHERE acl.grantee = monitor.oid) = ? \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_attribute attribute \
                 WHERE attribute.attrelid = relation.oid AND attribute.attnum > 0 \
                   AND NOT attribute.attisdropped AND attribute.attacl IS NOT NULL) \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_auth_members membership \
                 WHERE membership.member = runtime.oid OR membership.roleid = runtime.oid) \
               AND NOT pg_catalog.has_database_privilege(runtime.oid, database.oid, 'CREATE,TEMP') \
               AND NOT pg_catalog.pg_has_role(runtime.oid, namespace.nspowner, 'SET') \
               AND runtime.oid <> database.datdba \
               AND NOT EXISTS (SELECT 1 FROM pg_catalog.pg_proc function_ \
                 WHERE function_.pronamespace = namespace.oid AND function_.proowner = runtime.oid) \
             FROM pg_catalog.pg_class relation \
             JOIN pg_catalog.pg_namespace namespace ON namespace.oid = relation.relnamespace \
             JOIN pg_catalog.pg_roles runtime ON runtime.rolname = ? \
             JOIN pg_catalog.pg_roles monitor ON monitor.rolname = 'pg_monitor' \
             JOIN pg_catalog.pg_database database \
               ON database.datname = pg_catalog.current_database() \
             WHERE namespace.nspname = ? AND relation.relname = ?",
            crate::db_params![
                contract.read_only,
                contract.read_only,
                contract.read_only,
                contract.read_only,
                contract.read_only,
                true,
                expected_count,
                expected_monitor_count,
                runtime,
                &identity.schema.name.0,
                table,
            ],
        )
        .await?;
    let Some(row) = rows.next().await? else {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    };
    if !row.get::<bool>(0)? {
        return Err(RoomObservationSchemaError::UnsafeRuntime);
    }
    Ok(())
}
