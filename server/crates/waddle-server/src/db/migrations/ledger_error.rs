use thiserror::Error;

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
        "the migration ledger still holds {versions} version(s) of the retired migration \
         history, and WADDLE_DB_BASELINE_ACTION=apply never stamps; run the owner step once \
         without it (or with WADDLE_DB_BASELINE_ACTION=stamp) to record the schema baseline \
         on this database"
    )]
    RetiredLedger { versions: usize },

    #[error(
        "the schema baseline cannot be stamped: the ledger holds retired versions together \
         with v{version}"
    )]
    RetiredLedgerMixed { version: i64 },

    #[error(
        "the schema baseline cannot be stamped: retired migration(s) {missing:?} were never \
         applied, so an older release migrated this database last; run the final release that \
         carries the retired migration history (its ledger ends at v{last}) on it first"
    )]
    RetiredLedgerIncomplete { missing: Vec<i64>, last: i64 },

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
         schema exists (found table {table}), but the migration ledger is empty; startup \
         refuses to build the schema baseline over it; restore the _migrations table (or \
         intentionally start from an empty database)"
    )]
    SchemaWithoutLedger { table: String },

    #[error(
        "migration startup is refusing to continue rather than repairing the ledger: migration \
         v{missing} is missing, but found later applied migration v{applied_after}; expected \
         all prior migrations to be recorded"
    )]
    VersionGap { missing: i64, applied_after: i64 },
}

#[cfg(test)]
mod tests {
    use super::MigrationLedgerError;

    #[test]
    fn error_messages_describe_startup_refusal() {
        assert_eq!(
            MigrationLedgerError::UnknownVersion {
                version: 2007,
                description: "future migration".to_string(),
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: found unknown migration v2007 (future migration), but this binary expects every owned ledger version to exist in its migration catalog; this usually means an older binary is starting against a newer ledger"
        );
        assert_eq!(
            MigrationLedgerError::DescriptionMismatch {
                version: 2000,
                expected: "expected".to_string(),
                found: "found".to_string(),
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: migration v2000 description expected expected, found found"
        );
        assert_eq!(
            MigrationLedgerError::ChecksumMismatch {
                version: 2000,
                expected: "expected".to_string(),
                found: "found".to_string(),
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: migration v2000 checksum expected expected, found found"
        );
        assert_eq!(
            MigrationLedgerError::MissingChecksum { version: 2000 }.to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: migration v2000 checksum found NULL, expected a stored checksum because adoption is one-time"
        );
        assert_eq!(
            MigrationLedgerError::SchemaWithoutLedger {
                table: "users".to_string(),
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: the schema exists (found table users), but the migration ledger is empty; startup refuses to build the schema baseline over it; restore the _migrations table (or intentionally start from an empty database)"
        );
        assert_eq!(
            MigrationLedgerError::VersionGap {
                missing: 2001,
                applied_after: 2002,
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: migration v2001 is missing, but found later applied migration v2002; expected all prior migrations to be recorded"
        );
        assert_eq!(
            MigrationLedgerError::RetiredLedgerIncomplete {
                missing: vec![1036, 1037],
                last: 1037,
            }
            .to_string(),
            "the schema baseline cannot be stamped: retired migration(s) [1036, 1037] were never applied, so an older release migrated this database last; run the final release that carries the retired migration history (its ledger ends at v1037) on it first"
        );
    }
}
