use thiserror::Error;

use super::namespace::MigrationNamespace;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MigrationLedgerError {
    #[error("migration startup cannot identify a safe PostgreSQL target schema")]
    UnsafeTargetSchema,

    #[error("migration startup refuses a malformed or changed PostgreSQL ledger catalog")]
    UnsafeCatalog,

    #[error("migration startup refuses enabled PostgreSQL event triggers")]
    EnabledEventTrigger,

    #[error("migration startup timed out waiting for the PostgreSQL ledger relation lock")]
    RelationLockTimeout,

    #[error(
        "migration startup is refusing to continue rather than repairing the ledger: found \
         unknown migration v{version} ({description}), but this binary expects every owned \
         ledger version to exist in its migration catalog; this usually means an older binary \
         is starting against a newer ledger"
    )]
    UnknownVersion { version: i64, description: String },

    #[error(
        "migration startup is refusing to continue rather than repairing the ledger: migration \
         v{version} description expected {expected}, found {found}"
    )]
    DescriptionMismatch {
        version: i64,
        expected: String,
        found: String,
    },

    #[error(
        "migration startup is refusing to continue rather than repairing the ledger: migration \
         v{version} checksum expected {expected}, found {found}"
    )]
    ChecksumMismatch {
        version: i64,
        expected: String,
        found: String,
    },

    #[error(
        "migration startup is refusing to continue rather than repairing the ledger: migration \
         v{version} checksum found NULL, expected a stored checksum because adoption is one-time"
    )]
    MissingChecksum { version: i64 },

    #[error(
        "the schema baseline is selected, but the migration ledger still holds {versions} \
         retired version(s); run the owner step once with WADDLE_DB_BASELINE_ACTION=stamp to \
         record the baseline on this database"
    )]
    RetiredLedger { versions: usize },

    #[error(
        "the schema baseline cannot be stamped: the ledger holds retired versions together \
         with v{version}"
    )]
    RetiredLedgerMixed { version: i64 },

    #[error(
        "the schema baseline cannot be stamped: retired migration(s) {missing:?} were never \
         applied; run the last release that carries the migration history first"
    )]
    RetiredLedgerIncomplete { missing: Vec<i64> },

    #[error(
        "the schema baseline cannot be stamped: ledger version v{version} is not part of the \
         retired migration history"
    )]
    RetiredLedgerUnknownVersion { version: i64 },

    #[error(
        "the schema baseline cannot be stamped: ledger version v{version} does not carry the \
         retired history's description and checksum"
    )]
    RetiredLedgerMismatch { version: i64 },

    #[error(
        "the schema baseline cannot be stamped while {others} other session(s) or prepared \
         transaction(s) use the database"
    )]
    BaselineStampNotExclusive { others: i64 },

    #[error(
        "the schema baseline cannot be stamped: {count} object(s) differ from the schema the \
         migration history builds ({first})"
    )]
    BaselineCatalogMismatch { count: usize, first: String },

    #[error("the schema baseline stamp did not leave exactly the baseline row; nothing was kept")]
    BaselineStampUnverified,

    #[error("the embedded schema baseline artefacts are not valid")]
    BaselineArtefactInvalid,

    #[error("migration runtime attachment refuses duplicate ledger version v{version}")]
    DuplicateVersion { version: i64 },

    #[error("migration runtime attachment refuses an oversized migration ledger")]
    OversizedLedger,

    #[error("migration runtime attachment requires committed migration v{version}")]
    PendingMigration { version: i64 },

    #[error(
        "migration startup is refusing to continue rather than repairing the ledger: the \
         {namespace} schema exists (found table {table}), but the migration ledger has no \
         record of that namespace; startup refuses to re-run destructive initial migrations; \
         restore the _migrations table (or intentionally start from an empty database)"
    )]
    SchemaWithoutLedger {
        namespace: MigrationNamespace,
        table: String,
    },

    #[error(
        "migration startup is refusing to continue rather than repairing the ledger: {namespace} \
         migration v{missing} is missing, but found later applied migration v{applied_after}; \
         expected all prior namespace migrations to be recorded"
    )]
    VersionGap {
        namespace: MigrationNamespace,
        missing: i64,
        applied_after: i64,
    },
}

#[cfg(test)]
mod tests {
    use super::MigrationLedgerError;
    use crate::db::migrations::MigrationNamespace;

    #[test]
    fn error_messages_describe_startup_refusal() {
        assert_eq!(
            MigrationLedgerError::UnknownVersion {
                version: 1007,
                description: "future migration".to_string(),
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: found unknown migration v1007 (future migration), but this binary expects every owned ledger version to exist in its migration catalog; this usually means an older binary is starting against a newer ledger"
        );
        assert_eq!(
            MigrationLedgerError::DescriptionMismatch {
                version: 7,
                expected: "expected".to_string(),
                found: "found".to_string(),
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: migration v7 description expected expected, found found"
        );
        assert_eq!(
            MigrationLedgerError::ChecksumMismatch {
                version: 1007,
                expected: "expected".to_string(),
                found: "found".to_string(),
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: migration v1007 checksum expected expected, found found"
        );
        assert_eq!(
            MigrationLedgerError::MissingChecksum { version: 11 }.to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: migration v11 checksum found NULL, expected a stored checksum because adoption is one-time"
        );
        assert_eq!(
            MigrationLedgerError::SchemaWithoutLedger {
                namespace: MigrationNamespace::Waddle,
                table: "channels".to_string(),
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: the waddle schema exists (found table channels), but the migration ledger has no record of that namespace; startup refuses to re-run destructive initial migrations; restore the _migrations table (or intentionally start from an empty database)"
        );
        assert_eq!(
            MigrationLedgerError::SchemaWithoutLedger {
                namespace: MigrationNamespace::Global,
                table: "users".to_string(),
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: the global schema exists (found table users), but the migration ledger has no record of that namespace; startup refuses to re-run destructive initial migrations; restore the _migrations table (or intentionally start from an empty database)"
        );
        assert_eq!(
            MigrationLedgerError::VersionGap {
                namespace: MigrationNamespace::Global,
                missing: 6,
                applied_after: 7,
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: global migration v6 is missing, but found later applied migration v7; expected all prior namespace migrations to be recorded"
        );
    }
}
