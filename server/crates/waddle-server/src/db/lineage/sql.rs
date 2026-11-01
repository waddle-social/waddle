use crate::db::DatabaseDriver;

pub(crate) const LINEAGE_FORMAT: i64 = 1;

pub(super) const LINEAGE_COLUMNS: [&str; 10] = [
    "id",
    "format",
    "lineage_uuid",
    "deployment_uuid",
    "pg_system_identifier",
    "pg_database_oid",
    "pg_database_name",
    "pg_schema_oid",
    "pg_schema_name",
    "stamped_at",
];

pub(super) fn lineage_table_sql(driver: DatabaseDriver) -> &'static str {
    match driver {
        DatabaseDriver::Sqlite => {
            r#"
            CREATE TABLE IF NOT EXISTS _lineage (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                format INTEGER NOT NULL,
                lineage_uuid TEXT NOT NULL,
                deployment_uuid TEXT NOT NULL,
                pg_system_identifier TEXT,
                pg_database_oid TEXT,
                pg_database_name TEXT,
                pg_schema_oid TEXT,
                pg_schema_name TEXT,
                stamped_at TEXT NOT NULL DEFAULT (datetime('now'))
            )
            "#
        }
        DatabaseDriver::Postgres => {
            r#"
            CREATE TABLE IF NOT EXISTS _lineage (
                id INTEGER PRIMARY KEY CHECK (id = 1),
                format INTEGER NOT NULL,
                lineage_uuid TEXT NOT NULL,
                deployment_uuid TEXT NOT NULL,
                pg_system_identifier TEXT,
                pg_database_oid TEXT,
                pg_database_name TEXT,
                pg_schema_oid TEXT,
                pg_schema_name TEXT,
                stamped_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP
            )
            "#
        }
    }
}

pub(super) fn read_row_sql(table: &str) -> String {
    format!(
        "SELECT format, lineage_uuid, deployment_uuid, \
         pg_system_identifier, pg_database_oid, pg_database_name, pg_schema_oid, pg_schema_name \
         FROM {table} WHERE id = 1"
    )
}

pub(super) fn read_postgres_row_with_live_identity_sql(table: &str) -> String {
    format!(
        "SELECT l.format, l.lineage_uuid, \
    l.deployment_uuid, l.pg_system_identifier, l.pg_database_oid, l.pg_database_name, \
    l.pg_schema_oid, l.pg_schema_name, \
    (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
    (SELECT oid::text FROM pg_catalog.pg_database WHERE datname = current_database()), \
    current_database(), \
    (SELECT oid::text FROM pg_catalog.pg_namespace WHERE nspname = current_schema()), \
    current_schema() \
    FROM {table} l WHERE l.id = 1"
    )
}

pub(super) const READ_POSTGRES_LIVE_IDENTITY_SQL: &str = "SELECT \
    (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
    (SELECT oid::text FROM pg_catalog.pg_database WHERE datname = current_database()), \
    current_database(), \
    (SELECT oid::text FROM pg_catalog.pg_namespace WHERE nspname = current_schema()), \
    current_schema()";
