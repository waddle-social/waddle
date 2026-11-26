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

    #[error("migration runtime attachment refuses duplicate ledger version v{version}")]
    DuplicateVersion { version: i64 },

    #[error("migration runtime attachment refuses an oversized migration ledger")]
    OversizedLedger,

    #[error("migration runtime attachment requires committed migration v{version}")]
    PendingMigration { version: i64 },

    #[error(
        "migration startup is refusing to continue rather than repairing the ledger: the \
         schema exists (found table {table}), but the migration ledger is empty; startup \
         refuses to build the initial schema over it; restore the _migrations table (or \
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
                version: 7,
                description: "future migration".to_string(),
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: found unknown migration v7 (future migration), but this binary expects every owned ledger version to exist in its migration catalog; this usually means an older binary is starting against a newer ledger"
        );
        assert_eq!(
            MigrationLedgerError::DescriptionMismatch {
                version: 1,
                expected: "expected".to_string(),
                found: "found".to_string(),
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: migration v1 description expected expected, found found"
        );
        assert_eq!(
            MigrationLedgerError::ChecksumMismatch {
                version: 1,
                expected: "expected".to_string(),
                found: "found".to_string(),
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: migration v1 checksum expected expected, found found"
        );
        assert_eq!(
            MigrationLedgerError::MissingChecksum { version: 1 }.to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: migration v1 checksum found NULL, expected a stored checksum because adoption is one-time"
        );
        assert_eq!(
            MigrationLedgerError::SchemaWithoutLedger {
                table: "users".to_string(),
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: the schema exists (found table users), but the migration ledger is empty; startup refuses to build the initial schema over it; restore the _migrations table (or intentionally start from an empty database)"
        );
        assert_eq!(
            MigrationLedgerError::VersionGap {
                missing: 2,
                applied_after: 3,
            }
            .to_string(),
            "migration startup is refusing to continue rather than repairing the ledger: migration v2 is missing, but found later applied migration v3; expected all prior migrations to be recorded"
        );
    }
}
