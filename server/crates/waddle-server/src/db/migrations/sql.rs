use super::super::DatabaseDriver;

pub(super) fn migrations_table_sql(driver: DatabaseDriver) -> &'static str {
    match driver {
        DatabaseDriver::Sqlite => {
            r#"
            CREATE TABLE IF NOT EXISTS _migrations (
                version INTEGER PRIMARY KEY,
                description TEXT NOT NULL,
                checksum TEXT,
                applied_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#
        }
        DatabaseDriver::Postgres => {
            r#"
            CREATE TABLE IF NOT EXISTS _migrations (
                version BIGINT PRIMARY KEY,
                description TEXT NOT NULL,
                checksum TEXT,
                applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#
        }
    }
}
