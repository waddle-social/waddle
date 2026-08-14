//! PostgreSQL schema contracts adjacent to XEP-0359.
//!
//! XEP-0359 §3–4 requires a server which adds a stanza-id to preserve the
//! opaque client origin-id.  Sender and target scoping below is Waddle's local
//! deduplication policy, not an XEP-defined archive identity rule.

#[cfg(feature = "clustering")]
mod ingress_shadow_support;

use waddle_server::{
    db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner},
    ingress_substrate::PostgresIngressSubstrate,
};
use waddle_xmpp::ingress::{
    AliasOutcome, AliasResolution, MessageKey, NormalizedTarget, SemanticDigest,
    MAX_ORIGIN_ID_BYTES,
};
use waddle_xmpp_core::xep0359::OriginId;

#[cfg(feature = "clustering")]
use ingress_shadow_support::ShadowFixture;

#[tokio::test]
async fn origin_ids_are_opaque_bounded_and_preserved() {
    let Some(fixture) = Fixture::open("opaque").await else {
        return;
    };
    let sender = bare("romeo@example.com");
    let opaque = OriginId::new("not-a-uuid: client/chosen value");
    let digest = digest(1);
    let key = MessageKey::new();
    let mut tx = fixture.store.begin().await.expect("begin alias insert");
    assert_eq!(
        fixture
            .store
            .resolve_and_record_alias(
                &mut tx,
                &sender,
                &NormalizedTarget::Absent,
                &opaque,
                &digest,
                || key,
            )
            .await
            .expect("opaque origin id accepted"),
        AliasResolution::Aliased(AliasOutcome::Inserted(key))
    );
    tx.commit().await.expect("commit alias insert");

    let accepted = OriginId::new("a".repeat(MAX_ORIGIN_ID_BYTES));
    let mut tx = fixture.store.begin().await.expect("begin boundary insert");
    assert!(fixture
        .store
        .resolve_and_record_alias(
            &mut tx,
            &sender,
            &NormalizedTarget::Bare(bare("juliet@example.com")),
            &accepted,
            &digest,
            MessageKey::new,
        )
        .await
        .is_ok());
    tx.commit().await.expect("commit accepted boundary");

    let rejected = OriginId::new("b".repeat(MAX_ORIGIN_ID_BYTES + 1));
    let mut tx = fixture
        .store
        .begin()
        .await
        .expect("begin rejected boundary");
    assert!(fixture
        .store
        .resolve_and_record_alias(
            &mut tx,
            &sender,
            &NormalizedTarget::Full(full("juliet@example.com/phone")),
            &rejected,
            &digest,
            MessageKey::new,
        )
        .await
        .is_err());
    drop(tx); // the backend transaction rolls back unless committed
    fixture.close().await;
}

#[tokio::test]
async fn aliases_are_sender_and_target_scoped_without_silent_overwrite() {
    let Some(fixture) = Fixture::open("scoped").await else {
        return;
    };
    let origin = OriginId::new("same-client-id");
    let first = insert(
        &fixture,
        bare("romeo@example.com"),
        NormalizedTarget::Absent,
        &origin,
        digest(2),
    )
    .await;
    let second = insert(
        &fixture,
        bare("juliet@example.com"),
        NormalizedTarget::Absent,
        &origin,
        digest(2),
    )
    .await;
    assert_ne!(first, second, "different senders own distinct alias keys");

    for target in [
        NormalizedTarget::Bare(bare("room@example.com")),
        NormalizedTarget::Full(full("room@example.com/nick")),
    ] {
        let result = insert(
            &fixture,
            bare("romeo@example.com"),
            target,
            &origin,
            digest(2),
        )
        .await;
        assert_ne!(result, first, "target shape is part of Waddle's alias key");
    }

    let mut tx = fixture.store.begin().await.expect("begin conflict lookup");
    let conflict = fixture
        .store
        .resolve_and_record_alias(
            &mut tx,
            &bare("romeo@example.com"),
            &NormalizedTarget::Absent,
            &origin,
            &digest(3),
            MessageKey::new,
        )
        .await
        .expect("lookup completes");
    tx.commit().await.expect("commit conflict lookup");
    assert!(matches!(
        conflict,
        AliasResolution::Aliased(AliasOutcome::Conflict(_))
    ));
    assert_eq!(fixture.count("ingress_origin_aliases").await, 4);
    fixture.close().await;
}

#[tokio::test]
async fn same_digest_reuses_the_existing_alias_binding() {
    let Some(fixture) = Fixture::open("existing").await else {
        return;
    };
    let sender = bare("romeo@example.com");
    let target = NormalizedTarget::Bare(bare("juliet@example.com"));
    let origin = OriginId::new("same-digest-origin");
    let digest = digest(9);
    let key = MessageKey::new();

    let mut tx = fixture.store.begin().await.expect("begin alias insert");
    assert!(matches!(
        fixture
            .store
            .resolve_and_record_alias(&mut tx, &sender, &target, &origin, &digest, || key)
            .await
            .expect("insert origin alias"),
        AliasResolution::Aliased(AliasOutcome::Inserted(inserted)) if inserted == key
    ));
    tx.commit().await.expect("commit alias insert");

    let mut tx = fixture
        .store
        .begin()
        .await
        .expect("begin alias re-resolution");
    let resolved = fixture
        .store
        .resolve_and_record_alias(&mut tx, &sender, &target, &origin, &digest, MessageKey::new)
        .await
        .expect("re-resolve alias");
    tx.commit().await.expect("commit alias re-resolution");

    assert!(matches!(
        resolved,
        AliasResolution::Aliased(AliasOutcome::Existing(existing)) if existing == key
    ));
    assert_eq!(
        fixture.count("ingress_origin_aliases").await,
        1,
        "same-digest retries must preserve the first alias row"
    );
    fixture.close().await;
}

/// XEP-0359 §4 requires a server-added stanza-id path to preserve the opaque
/// client origin-id. The production shadow worker must therefore resolve a
/// repeated live-shaped origin-id to Inserted, Existing, then Conflict as the
/// message digest diverges.
#[cfg(feature = "clustering")]
#[tokio::test]
async fn shadow_pipeline_records_insert_existing_and_conflict_alias_outcomes() {
    let Some(fixture) = ShadowFixture::open("xep0359_pipeline").await else {
        return;
    };

    fixture
        .enqueue(fixture.submission_with_intents(
            1,
            Some("opaque-origin"),
            "alias body",
            Vec::new(),
        ))
        .await;
    fixture.wait_for_frontier(1).await;
    let inserted_key = fixture
        .message_key_for_ordinal(1)
        .await
        .expect("inserted origin-id should bind ordinal one");

    fixture
        .enqueue(fixture.submission_with_intents(
            2,
            Some("opaque-origin"),
            "alias body",
            Vec::new(),
        ))
        .await;
    fixture.wait_for_frontier(2).await;
    let existing_key = fixture
        .message_key_for_ordinal(2)
        .await
        .expect("existing origin-id should bind ordinal two");

    fixture
        .enqueue(fixture.submission_with_intents(
            3,
            Some("opaque-origin"),
            "different alias body",
            Vec::new(),
        ))
        .await;
    fixture.wait_for_frontier(3).await;

    assert_eq!(
        inserted_key, existing_key,
        "same-digest retries must reuse the original canonical message key"
    );
    assert_eq!(
        count_table(&fixture, "ingress_messages").await,
        1,
        "alias conflict must not mint a second canonical message"
    );
    assert_eq!(
        count_table(&fixture, "ingress_origin_aliases").await,
        1,
        "the opaque origin-id must preserve a single alias binding"
    );
    assert_eq!(
        count_table(&fixture, "ingress_sm_refs").await,
        2,
        "conflicting retries must not bind an SM ref"
    );
    assert!(
        fixture.message_key_for_ordinal(3).await.is_none(),
        "the conflicting retry must not claim handled ordinal three"
    );

    fixture.close().await;
}

#[cfg(feature = "clustering")]
async fn count_table(fixture: &ShadowFixture, table: &str) -> i64 {
    let conn = fixture.db.guard().await.expect("database connection");
    let mut rows = conn
        .query(&format!("SELECT COUNT(*) FROM {table}"), ())
        .await
        .expect("count rows");
    rows.next()
        .await
        .expect("read count row")
        .expect("count row exists")
        .get(0)
        .expect("decode count")
}

async fn insert(
    fixture: &Fixture,
    sender: jid::BareJid,
    target: NormalizedTarget,
    origin: &OriginId,
    digest: SemanticDigest,
) -> MessageKey {
    let key = MessageKey::new();
    let mut tx = fixture.store.begin().await.expect("begin alias insert");
    let resolution = fixture
        .store
        .resolve_and_record_alias(&mut tx, &sender, &target, origin, &digest, || key)
        .await
        .expect("insert scoped alias");
    tx.commit().await.expect("commit alias insert");
    match resolution {
        AliasResolution::Aliased(AliasOutcome::Inserted(key)) => key,
        _ => panic!("fresh scoped alias must insert"),
    }
}

fn digest(byte: u8) -> SemanticDigest {
    SemanticDigest::from_storage(1, [byte; 32]).expect("valid fixture digest")
}

fn bare(value: &str) -> jid::BareJid {
    value.parse().expect("valid fixture bare JID")
}

fn full(value: &str) -> jid::FullJid {
    value.parse().expect("valid fixture full JID")
}

struct Fixture {
    store: PostgresIngressSubstrate,
    db: Database,
    admin: sqlx::PgPool,
    schema: String,
}

impl Fixture {
    async fn open(name: &str) -> Option<Self> {
        let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (XEP-0359 schema)");
            return None;
        };
        let schema = format!(
            "waddle_test_xep0359_{name}_{}",
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
            "xep0359-schema-test",
            &DatabaseConfig::new(DatabaseDriver::Postgres, schema_url(&database_url, &schema)),
        )
        .await
        .expect("open schema database");
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("apply migrations");
        let store = PostgresIngressSubstrate::open(db.clone()).expect("open ingress store");
        Some(Self {
            store,
            db,
            admin,
            schema,
        })
    }

    async fn count(&self, table: &str) -> i64 {
        let conn = self.db.guard().await.expect("database guard");
        let mut rows = conn
            .query(&format!("SELECT COUNT(*) FROM {table}"), ())
            .await
            .expect("count rows");
        rows.next()
            .await
            .expect("read count")
            .expect("count row")
            .get(0)
            .expect("decode count")
    }

    async fn close(self) {
        drop(self.store);
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
