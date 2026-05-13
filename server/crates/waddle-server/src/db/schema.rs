use super::{Database, DatabaseDriver, DatabaseError};

pub fn i64_sql_type(driver: DatabaseDriver) -> &'static str {
    match driver {
        DatabaseDriver::Postgres => "BIGINT",
        DatabaseDriver::Sqlite => "INTEGER",
    }
}

pub async fn widen_postgres_i64_column_to_bigint(
    db: &Database,
    table: &'static str,
    column: &'static str,
) -> Result<(), DatabaseError> {
    if !matches!(db.driver(), DatabaseDriver::Postgres) {
        return Ok(());
    }

    validate_sql_identifier(table)?;
    validate_sql_identifier(column)?;

    let conn = db.guard().await?;
    let mut rows = conn
        .query(
            "SELECT data_type \
             FROM information_schema.columns \
             WHERE table_schema = current_schema() \
               AND table_name = ? \
               AND column_name = ?",
            crate::db_params![table, column],
        )
        .await?;
    let current_type: Option<String> = match rows.next().await? {
        Some(row) => row.get(0)?,
        None => None,
    };
    let needs_widen = current_type
        .as_deref()
        .is_some_and(|data_type| !data_type.eq_ignore_ascii_case("bigint"));
    if needs_widen {
        conn.execute(
            &format!("ALTER TABLE {table} ALTER COLUMN {column} TYPE BIGINT"),
            (),
        )
        .await?;
    }
    Ok(())
}

fn validate_sql_identifier(identifier: &str) -> Result<(), DatabaseError> {
    let valid = !identifier.is_empty()
        && identifier
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_');
    if valid {
        Ok(())
    } else {
        Err(DatabaseError::QueryFailed(format!(
            "invalid SQL identifier: {identifier}"
        )))
    }
}
