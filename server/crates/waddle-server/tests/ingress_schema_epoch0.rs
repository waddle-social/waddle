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
