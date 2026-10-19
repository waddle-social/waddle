use super::*;

const LEGACY_INSERT: &str = "INSERT INTO mam_messages (id, room_jid, timestamp, from_jid, to_jid, body) VALUES ($1, $2, $3, 'sender@example.org', 'room@example.org', 'body')";
const EXPECTED: &[(&str, i64)] = &[("id-a", 1), ("id-b", 2), ("id-c", 3)];
const FIXTURES: &[(&str, &str)] = &[
    ("id-c", "2026-01-02T00:00:00Z"),
    ("id-b", "2026-01-01T00:00:00Z"),
    ("id-a", "2026-01-01T00:00:00Z"),
];

#[tokio::test]
async fn sqlite_legacy_ordinal_backfill_is_restart_safe_and_preserves_high_water() {
    let file = tempfile::NamedTempFile::new().expect("SQLite file");
    let options = sqlx::sqlite::SqliteConnectOptions::new().filename(file.path());
    let pool = sqlx::sqlite::SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(options)
        .await
        .expect("SQLite pool");
    let legacy_schema = SQLITE_MAM_SCHEMA.replace("    archive_seq INTEGER NOT NULL,\n", "");
    execute_sqlite_batch(&pool, &legacy_schema)
        .await
        .expect("legacy schema");
    for (id, timestamp) in FIXTURES {
        sqlx::query(LEGACY_INSERT)
            .bind(id)
            .bind("room@example.org")
            .bind(timestamp)
            .execute(&pool)
            .await
            .expect("legacy row");
    }
    ensure_sqlite_schema(&pool).await.expect("backfill");
    assert_sqlite_ordinals(&pool).await;
    let counter: i64 = sqlx::query_scalar(
        "SELECT next_seq FROM mam_archive_sequences WHERE archive_jid = 'room@example.org'",
    )
    .fetch_one(&pool)
    .await
    .expect("counter");
    assert_eq!(counter, 3);
    let columns = sqlx::query("PRAGMA table_info(mam_messages)")
        .fetch_all(&pool)
        .await
        .expect("columns");
    let ordinal = columns
        .iter()
        .find(|row| row.get::<String, _>("name") == "archive_seq")
        .expect("ordinal column");
    assert_eq!(ordinal.get::<i64, _>("notnull"), 1);
    sqlx::query("UPDATE mam_archive_sequences SET next_seq = 10")
        .execute(&pool)
        .await
        .expect("high water");
    ensure_sqlite_schema(&pool)
        .await
        .expect("second schema initialization");
    assert_sqlite_ordinals(&pool).await;
    let counter: i64 = sqlx::query_scalar(
        "SELECT next_seq FROM mam_archive_sequences WHERE archive_jid = 'room@example.org'",
    )
    .fetch_one(&pool)
    .await
    .expect("counter");
    assert_eq!(counter, 10);
}

async fn assert_sqlite_ordinals(pool: &SqlitePool) {
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT id, archive_seq FROM mam_messages ORDER BY archive_seq")
            .fetch_all(pool)
            .await
            .expect("ordinals");
    assert_eq!(
        rows,
        EXPECTED
            .iter()
            .map(|(id, seq)| ((*id).to_owned(), *seq))
            .collect::<Vec<_>>()
    );
}

#[tokio::test]
async fn sqlite_partial_backfill_reranks_all_rows_of_incomplete_archives() {
    let pool = SqlitePool::connect("sqlite::memory:")
        .await
        .expect("SQLite pool");
    let legacy_schema =
        SQLITE_MAM_SCHEMA.replace("archive_seq INTEGER NOT NULL", "archive_seq INTEGER");
    execute_sqlite_batch(&pool, &legacy_schema)
        .await
        .expect("legacy schema");
    for (id, timestamp) in FIXTURES {
        sqlx::query(LEGACY_INSERT)
            .bind(id)
            .bind("room@example.org")
            .bind(timestamp)
            .execute(&pool)
            .await
            .expect("legacy row");
    }
    sqlx::query("UPDATE mam_messages SET archive_seq = 1 WHERE id = 'id-c'")
        .execute(&pool)
        .await
        .expect("interrupted legacy backfill");
    ensure_sqlite_schema(&pool).await.expect("resume backfill");
    assert_sqlite_ordinals(&pool).await;
}

#[tokio::test]
async fn postgres_legacy_ordinal_backfill_is_restart_safe_and_preserves_high_water() {
    let Ok(url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
        eprintln!("skipping Postgres MAM ordinal schema test: WADDLE_TEST_POSTGRES_URL is unset");
        return;
    };
    let pool = sqlx::postgres::PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("Postgres pool");
    let schema = format!("mam_ordinals_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&pool)
        .await
        .expect("isolated schema");
    sqlx::query(&format!("SET search_path TO {schema}"))
        .execute(&pool)
        .await
        .expect("search path");
    let legacy_schema = POSTGRES_MAM_SCHEMA.replace("    archive_seq BIGINT NOT NULL,\n", "");
    {
        let mut conn = pool.acquire().await.expect("connection");
        execute_postgres_batch(&mut conn, &legacy_schema)
            .await
            .expect("legacy schema");
    }
    for (id, timestamp) in FIXTURES {
        let timestamp = chrono::DateTime::parse_from_rfc3339(timestamp).expect("timestamp");
        sqlx::query(LEGACY_INSERT)
            .bind(id)
            .bind("room@example.org")
            .bind(timestamp)
            .execute(&pool)
            .await
            .expect("legacy row");
    }
    // Simulate a restart after nullable-column addition and partial historical data.
    sqlx::query("ALTER TABLE mam_messages ADD COLUMN archive_seq BIGINT")
        .execute(&pool)
        .await
        .expect("nullable ordinal");
    sqlx::query("UPDATE mam_messages SET archive_seq = 1 WHERE id = 'id-c'")
        .execute(&pool)
        .await
        .expect("partial backfill");
    ensure_postgres_schema(&pool).await.expect("backfill");
    let rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT id, archive_seq FROM mam_messages ORDER BY archive_seq")
            .fetch_all(&pool)
            .await
            .expect("ordinals");
    assert_eq!(
        rows,
        EXPECTED
            .iter()
            .map(|(id, seq)| ((*id).to_owned(), *seq))
            .collect::<Vec<_>>()
    );
    let counter: i64 = sqlx::query_scalar(
        "SELECT next_seq FROM mam_archive_sequences WHERE archive_jid = 'room@example.org'",
    )
    .fetch_one(&pool)
    .await
    .expect("counter");
    assert_eq!(counter, 3);
    let nullable: String = sqlx::query_scalar("SELECT is_nullable FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'mam_messages' AND column_name = 'archive_seq'")
        .fetch_one(&pool).await.expect("nullability");
    assert_eq!(nullable, "NO");
    sqlx::query("UPDATE mam_archive_sequences SET next_seq = 10")
        .execute(&pool)
        .await
        .expect("high water");
    ensure_postgres_schema(&pool)
        .await
        .expect("idempotent initialization");
    let second_rows: Vec<(String, i64)> =
        sqlx::query_as("SELECT id, archive_seq FROM mam_messages ORDER BY archive_seq")
            .fetch_all(&pool)
            .await
            .expect("ordinals");
    assert_eq!(rows, second_rows);
    let counter: i64 = sqlx::query_scalar(
        "SELECT next_seq FROM mam_archive_sequences WHERE archive_jid = 'room@example.org'",
    )
    .fetch_one(&pool)
    .await
    .expect("counter");
    assert_eq!(counter, 10);
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&pool)
        .await
        .expect("drop schema");
}
