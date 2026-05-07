use super::super::DatabaseDriver;

/// Represents a single database migration with driver-specific SQL.
///
/// Each migration carries separate SQL for SQLite and Postgres so the
/// runner can apply the correct dialect without any runtime rewriting.
#[derive(Debug, Clone)]
pub struct Migration {
    /// Version number (must be unique and incrementing)
    pub version: i64,
    /// Description of what this migration does
    pub description: String,
    /// SQL to execute on SQLite
    pub sql_sqlite: &'static str,
    /// SQL to execute on Postgres
    pub sql_postgres: &'static str,
}

impl Migration {
    /// Return the SQL appropriate for the given driver.
    pub fn sql_for(&self, driver: DatabaseDriver) -> &'static str {
        match driver {
            DatabaseDriver::Sqlite => self.sql_sqlite,
            DatabaseDriver::Postgres => self.sql_postgres,
        }
    }
}
