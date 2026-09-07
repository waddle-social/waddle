//! Expand-only compatibility checks for the inert (epoch 0) ingress schema.

use waddle_server::db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner};

#[tokio::test]
async fn full_catalog_keeps_old_writes_and_new_ingress_writes_available_at_epoch_zero() {
    let Some(fixture) = Fixture::open("compatibility").await else {
        return;
    };
    // These are representative plain-SQL writes from a binary that does not
    // know the ingress GUC contract.  No proof GUC is set anywhere in this test.
    for statement in [
        "INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at) VALUES ('romeo@example.com', 'romeo', 'romeo', now()::text, now()::text)",
        "INSERT INTO channels (id, name) VALUES ('general', 'General')",
        "INSERT INTO messages (id, channel_id, author_jid, content) VALUES ('old-message', 'general', 'romeo@example.com', 'still works')",
        "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES ('00000000-0000-0000-0000-000000000001', 1, decode(repeat('00', 32), 'hex'))",
        "INSERT INTO ingress_deliveries (delivery_key, message_key) VALUES ('00000000-0000-0000-0000-000000000002', '00000000-0000-0000-0000-000000000001')",
    ] {
        fixture
            .db
            .guard()
            .await
            .expect("database guard")
            .execute(statement, ())
            .await
            .expect("epoch-0 write succeeds");
    }
    fixture.close().await;
}

struct Fixture {
    db: Database,
    admin: sqlx::PgPool,
    schema: String,
}

impl Fixture {
    async fn open(name: &str) -> Option<Self> {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (epoch-0 schema)");
            return None;
        };
        let schema = format!(
            "waddle_test_epoch0_{name}_{}",
            uuid::Uuid::new_v4().simple()
        );
        let admin = sqlx::PgPool::connect(&database_url)
            .await
            .expect("connect PostgreSQL");
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create schema");
        let db = Database::from_config(
            "epoch-zero-schema-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, schema_url(&database_url, &schema)),
        )
        .await
        .expect("open schema database");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("apply complete catalog");
        Some(Self { db, admin, schema })
    }

    async fn close(self) {
        drop(self.db);
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .expect("drop schema");
    }
}

fn schema_url(database_url: &str, schema: &str) -> String {
    let mut url = url::Url::parse(database_url).expect("parse PostgreSQL URL");
    let values: Vec<(String, String)> = url
        .query_pairs()
        .filter(|(key, _)| key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(values.iter().map(|(key, value)| (key, value)))
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}

#[tokio::test]
async fn sqlite_v1012_ingress_schema_constraints() {
    let db = Database::in_memory("v1012-schema")
        .await
        .expect("SQLite database");
    MigrationRunner::single()
        .run(&db)
        .await
        .expect("migrate SQLite");
    assert_v1012_constraints(&db).await;
    assert_count(&db, "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name IN ('ingress_protocol_epoch', 'ingress_messages', 'ingress_origin_aliases', 'ingress_sm_refs', 'ingress_deliveries', 'ingress_sm_streams', 'ingress_effect_intents', 'ingress_effect_receipts')", 8).await;
    assert_count(
        &db,
        "SELECT COUNT(*) FROM sqlite_master WHERE type = 'trigger' AND tbl_name LIKE 'ingress_%'",
        0,
    )
    .await;
    assert_count(&db, "SELECT COUNT(*) FROM pragma_foreign_key_list('ingress_effect_receipts') WHERE \"table\" = 'ingress_effect_intents' AND on_delete = 'CASCADE'", 3).await;
}

#[tokio::test]
async fn postgres_v1012_ingress_schema_catalog_and_constraints() {
    let Some(fixture) = Fixture::open("v1012_catalog").await else {
        return;
    };
    assert_v1012_constraints(&fixture.db).await;
    assert_count(&fixture.db, "SELECT COUNT(*) FROM information_schema.columns WHERE table_schema = current_schema() AND ((table_name = 'ingress_messages' AND column_name = 'envelope_version' AND data_type = 'smallint' AND is_nullable = 'YES') OR (table_name = 'ingress_messages' AND column_name = 'envelope' AND data_type = 'bytea' AND is_nullable = 'YES') OR (table_name = 'ingress_sm_streams' AND column_name = 'checkpoint_h' AND data_type = 'bigint' AND is_nullable = 'NO') OR (table_name = 'ingress_sm_refs' AND column_name = 'wire_h' AND data_type = 'bigint' AND is_nullable = 'NO'))", 4).await;
    assert_count(&fixture.db, "SELECT COUNT(*) FROM pg_constraint WHERE conrelid = 'ingress_sm_refs'::regclass AND contype = 'u' AND pg_get_constraintdef(oid) = 'UNIQUE (sm_ingress_id, wire_h)'", 1).await;
    assert_count(&fixture.db, "SELECT COUNT(*) FROM pg_constraint WHERE conrelid = 'ingress_effect_receipts'::regclass AND contype = 'f' AND confrelid = 'ingress_effect_intents'::regclass AND confdeltype = 'c' AND array_length(conkey, 1) = 3", 1).await;
    assert_count(&fixture.db, "SELECT COUNT(*) FROM pg_constraint WHERE conrelid = 'ingress_effect_receipts'::regclass AND contype = 'p' AND array_length(conkey, 1) = 3", 1).await;
    assert_count(&fixture.db, "SELECT COUNT(*) FROM pg_trigger WHERE tgrelid = 'ingress_effect_receipts'::regclass AND NOT tgisinternal AND tgenabled = 'A' AND ((tgname = 'ingress_effect_receipts_epoch_guard_dml' AND tgfoid = 'waddle_ingress_epoch_guard()'::regprocedure AND tgtype = 30) OR (tgname = 'ingress_effect_receipts_epoch_guard_truncate' AND tgfoid = 'waddle_ingress_truncate_guard()'::regprocedure AND tgtype = 34))", 2).await;
    assert_count(&fixture.db, "SELECT COUNT(*) FROM ingress_epoch_guard_manifest WHERE table_name = 'ingress_effect_receipts'", 1).await;
    assert_count(&fixture.db, "SELECT COUNT(*) WHERE has_table_privilege('pg_monitor', 'ingress_effect_receipts', 'SELECT')", 1).await;
    fixture.close().await;
}

async fn assert_count(db: &Database, sql: &str, expected: i64) {
    let conn = db.guard().await.expect("database guard");
    let mut rows = conn.query(sql, ()).await.expect("catalog query");
    let count: i64 = rows
        .next()
        .await
        .expect("read count")
        .expect("count row")
        .get(0)
        .expect("decode count");
    assert_eq!(count, expected, "{sql}");
}

async fn assert_v1012_constraints(db: &Database) {
    let conn = db.guard().await.expect("database guard");
    let insert_message = match db.driver() {
        DatabaseDriver::Postgres => "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES ('00000000-0000-0000-0000-000000000011', 1, decode(repeat('00', 32), 'hex'))",
        DatabaseDriver::Sqlite => "INSERT INTO ingress_messages (message_key, digest_version, digest) VALUES ('00000000-0000-0000-0000-000000000011', 1, zeroblob(32))",
    };
    conn.execute(insert_message, ())
        .await
        .expect("insert canonical row");
    for sql in [
        "UPDATE ingress_messages SET envelope_version = 1",
        "UPDATE ingress_messages SET envelope_version = 2, envelope = NULL",
        "INSERT INTO ingress_sm_streams (sm_ingress_id, stream_id, checkpoint_h) VALUES ('00000000-0000-0000-0000-000000000012', 'bad-low', -1)",
        "INSERT INTO ingress_sm_streams (sm_ingress_id, stream_id, checkpoint_h) VALUES ('00000000-0000-0000-0000-000000000012', 'bad-high', 4294967296)",
        "INSERT INTO ingress_sm_refs (sm_ingress_id, ingress_ordinal, wire_h, message_key) VALUES ('00000000-0000-0000-0000-000000000012', '1', -1, '00000000-0000-0000-0000-000000000011')",
        "INSERT INTO ingress_sm_refs (sm_ingress_id, ingress_ordinal, wire_h, message_key) VALUES ('00000000-0000-0000-0000-000000000012', '1', 4294967296, '00000000-0000-0000-0000-000000000011')",
    ] {
        assert!(conn.execute(sql, ()).await.is_err(), "constraint must reject: {sql}");
    }
    conn.execute("INSERT INTO ingress_sm_streams (sm_ingress_id, stream_id) VALUES ('00000000-0000-0000-0000-000000000012', 'valid')", ()).await.expect("default checkpoint");
    conn.execute("INSERT INTO ingress_sm_refs (sm_ingress_id, ingress_ordinal, wire_h, message_key) VALUES ('00000000-0000-0000-0000-000000000012', '1', 4294967295, '00000000-0000-0000-0000-000000000011')", ()).await.expect("u32 maximum binding");
    assert!(conn.execute("INSERT INTO ingress_sm_refs (sm_ingress_id, ingress_ordinal, wire_h, message_key) VALUES ('00000000-0000-0000-0000-000000000012', '2', 4294967295, '00000000-0000-0000-0000-000000000011')", ()).await.is_err(), "wire binding is unique independently of ordinal");
    for sql in [
        "UPDATE ingress_messages SET envelope = ?",
        "UPDATE ingress_messages SET envelope_version = 2, envelope = ?",
    ] {
        assert!(
            conn.execute(sql, waddle_server::db_params![vec![1_u8]])
                .await
                .is_err(),
            "envelope constraints reject: {sql}"
        );
    }
    conn.execute(
        "UPDATE ingress_messages SET envelope_version = 1, envelope = ?",
        waddle_server::db_params![vec![1_u8]],
    )
    .await
    .expect("version-one envelope");
    let receipt_sql = "INSERT INTO ingress_effect_receipts (message_key, kind, semantic_identity_hash) VALUES ('00000000-0000-0000-0000-000000000011', 0, ?)";
    assert!(
        conn.execute(receipt_sql, waddle_server::db_params![vec![0_u8; 32]])
            .await
            .is_err(),
        "receipt requires the exact intent"
    );
    conn.execute("INSERT INTO ingress_effect_intents (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) VALUES ('00000000-0000-0000-0000-000000000011', '0', 0, ?, 1, ?)", waddle_server::db_params![vec![0_u8; 32], vec![1_u8]]).await.expect("intent");
    conn.execute(receipt_sql, waddle_server::db_params![vec![0_u8; 32]])
        .await
        .expect("receipt");
    assert!(
        conn.execute(receipt_sql, waddle_server::db_params![vec![0_u8; 32]])
            .await
            .is_err(),
        "receipt primary key is unique"
    );
    conn.execute("DELETE FROM ingress_sm_refs", ())
        .await
        .expect("remove references");
    conn.execute("DELETE FROM ingress_messages", ())
        .await
        .expect("cascade canonical deletion");
    drop(conn);
    assert_count(db, "SELECT COUNT(*) FROM ingress_effect_intents", 0).await;
    assert_count(db, "SELECT COUNT(*) FROM ingress_effect_receipts", 0).await;
    assert_count(
        db,
        "SELECT COUNT(*) FROM ingress_protocol_epoch WHERE id = 1 AND epoch = 0",
        1,
    )
    .await;
    assert_count(
        db,
        "SELECT COUNT(*) FROM ingress_sm_streams WHERE checkpoint_h = 0",
        1,
    )
    .await;
}
