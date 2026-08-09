use sha2::{Digest, Sha256};

use super::super::DatabaseDriver;
use super::Migration;

/// Stable checksum for the SQL payload a given driver will actually execute.
pub fn migration_checksum(migration: &Migration, driver: DatabaseDriver) -> String {
    hex::encode(Sha256::digest(migration.sql_for(driver).as_bytes()))
}

#[cfg(test)]
mod tests {
    use super::migration_checksum;
    use crate::db::{migrations::Migration, DatabaseDriver};

    fn test_migration(sql_sqlite: &'static str, sql_postgres: &'static str) -> Migration {
        Migration {
            version: 42,
            description: "test migration".to_string(),
            sql_sqlite,
            sql_postgres,
        }
    }

    #[test]
    fn checksum_matches_golden_sha256_for_sqlite_and_postgres() {
        let migration = test_migration(
            "CREATE TABLE demo (id INTEGER);",
            "CREATE TABLE demo (id BIGINT);",
        );

        assert_eq!(
            migration_checksum(&migration, DatabaseDriver::Sqlite),
            "221b3d0c3f1e3eeee3988956e112ec8b8c844000b2180695379c05dd85b2c995"
        );
        assert_eq!(
            migration_checksum(&migration, DatabaseDriver::Postgres),
            "636c7348e4ceea63210c94ed0ceeca0e66aca42aaac3a0a6c666b7129ba43eee"
        );
    }

    #[test]
    fn checksum_uses_only_the_active_driver_sql() {
        let baseline = Migration {
            version: 42,
            description: "baseline".to_string(),
            sql_sqlite: "CREATE TABLE demo (id INTEGER);",
            sql_postgres: "CREATE TABLE demo (id BIGINT);",
        };
        let sqlite_inactive_changed = Migration {
            version: 999,
            description: "changed metadata".to_string(),
            sql_sqlite: "CREATE TABLE demo (id INTEGER);",
            sql_postgres: "ALTER TABLE demo ADD COLUMN note TEXT;",
        };
        let sqlite_active_changed = Migration {
            version: 999,
            description: "changed metadata".to_string(),
            sql_sqlite: "ALTER TABLE demo ADD COLUMN note TEXT;",
            sql_postgres: "CREATE TABLE demo (id BIGINT);",
        };

        assert_eq!(
            migration_checksum(&baseline, DatabaseDriver::Sqlite),
            migration_checksum(&sqlite_inactive_changed, DatabaseDriver::Sqlite)
        );
        assert_ne!(
            migration_checksum(&baseline, DatabaseDriver::Sqlite),
            migration_checksum(&sqlite_active_changed, DatabaseDriver::Sqlite)
        );
        assert_ne!(
            migration_checksum(&baseline, DatabaseDriver::Postgres),
            migration_checksum(&sqlite_inactive_changed, DatabaseDriver::Postgres)
        );
        assert_eq!(
            migration_checksum(&baseline, DatabaseDriver::Postgres),
            migration_checksum(&sqlite_active_changed, DatabaseDriver::Postgres)
        );
    }
}
