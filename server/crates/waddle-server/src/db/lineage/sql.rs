use crate::db::DatabaseDriver;

pub(super) const LINEAGE_FORMAT: i64 = 1;

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

pub(super) const READ_ROW_SQL: &str = "SELECT format, lineage_uuid, deployment_uuid, \
    pg_system_identifier, pg_database_oid, pg_database_name, pg_schema_oid, pg_schema_name \
    FROM _lineage WHERE id = 1";

pub(super) const READ_POSTGRES_ROW_WITH_LIVE_IDENTITY_SQL: &str =
    "SELECT l.format, l.lineage_uuid, \
    l.deployment_uuid, l.pg_system_identifier, l.pg_database_oid, l.pg_database_name, \
    l.pg_schema_oid, l.pg_schema_name, \
    (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
    (SELECT oid::text FROM pg_catalog.pg_database WHERE datname = current_database()), \
    current_database(), \
    (SELECT oid::text FROM pg_catalog.pg_namespace WHERE nspname = current_schema()), \
    current_schema() \
    FROM _lineage l WHERE l.id = 1";

pub(super) const READ_POSTGRES_LIVE_IDENTITY_SQL: &str = "SELECT \
    (SELECT system_identifier::text FROM pg_catalog.pg_control_system()), \
    (SELECT oid::text FROM pg_catalog.pg_database WHERE datname = current_database()), \
    current_database(), \
    (SELECT oid::text FROM pg_catalog.pg_namespace WHERE nspname = current_schema()), \
    current_schema()";
