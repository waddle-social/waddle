use thiserror::Error;

use super::namespace::MigrationNamespace;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum MigrationLedgerError {
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
