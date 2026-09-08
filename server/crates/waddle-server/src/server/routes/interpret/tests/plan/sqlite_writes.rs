//! Mutation tripwire for concrete notification stores and the shared DB actor.
use crate::db::Database;

/// Record and ignore writes, rather than returning an error an interpreter arm
/// could swallow. The audit table is deliberately excluded from the tripwire.
pub(super) async fn install(database: &Database) {
    let connection = database.guard().await.expect("database guard");
    connection
        .execute(
            "CREATE TABLE plan_write_attempts (target TEXT NOT NULL, operation TEXT NOT NULL)",
            (),
        )
        .await
        .expect("create mutation audit");
    let mut rows = connection
        .query("SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%' AND name != 'plan_write_attempts'", ())
        .await
        .expect("enumerate application tables");
    let mut tables = Vec::new();
    while let Some(row) = rows.next().await.expect("table row") {
        tables.push(row.get::<String>(0).expect("table name"));
    }
    assert!(tables
        .iter()
        .any(|table| table == "notification_candidates"));
    assert!(tables.iter().any(|table| table == "notification_activity"));
    assert!(tables
        .iter()
        .any(|table| table == "link_preview_media_refs"));
    for (index, table) in tables.into_iter().enumerate() {
        // Schema-derived identifiers are quoted independently from SQL values.
        let identifier = table.replace('"', "\"\"");
        let literal = table.replace('\'', "''");
        for operation in ["INSERT", "UPDATE", "DELETE"] {
            let sql = format!(
                "CREATE TRIGGER plan_write_{index}_{operation} BEFORE {operation} ON \"{identifier}\" BEGIN INSERT INTO plan_write_attempts VALUES ('{literal}', '{operation}'); SELECT RAISE(IGNORE); END"
            );
            connection
                .execute(&sql, ())
                .await
                .expect("install tripwire");
        }
    }
    // Calibrate against an existing seeded row: a successful SQL return must
    // still be observable as a write attempt, even though RAISE(IGNORE) blocks it.
    connection
        .execute("UPDATE users SET display_name = display_name", ())
        .await
        .expect("calibrate tripwire");
    let mut audit = connection
        .query("SELECT COUNT(*) FROM plan_write_attempts", ())
        .await
        .expect("calibration audit");
    let attempts: i64 = audit
        .next()
        .await
        .expect("audit row")
        .expect("count row")
        .get(0)
        .expect("count");
    assert!(attempts > 0, "tripwire must observe attempted writes");
    connection
        .execute("DELETE FROM plan_write_attempts", ())
        .await
        .expect("reset calibration audit");
}

pub(super) async fn assert_untouched(database: &Database) {
    let connection = database.guard().await.expect("database guard");
    let mut rows = connection
        .query("SELECT target, operation FROM plan_write_attempts", ())
        .await
        .expect("mutation audit");
    let mut attempts = Vec::new();
    while let Some(row) = rows.next().await.expect("audit row") {
        attempts.push((
            row.get::<String>(0).expect("target"),
            row.get::<String>(1).expect("operation"),
        ));
    }
    assert!(
        attempts.is_empty(),
        "planning attempted database writes: {attempts:?}"
    );
}
