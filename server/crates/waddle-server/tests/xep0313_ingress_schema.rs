//! PostgreSQL schema contracts adjacent to XEP-0313.
//!
//! `MessageKey` is Waddle's internal canonical association key.  It is not
//! the XEP-0313 archive UID: XEP-0313 §6.3 defines the MAM `<result/>` id as
//! the archive identifier and clients must not rely on stanza-id for it.

#[cfg(feature = "clustering")]
mod ingress_shadow_support;

use chrono::{Duration, Utc};
use jid::Jid;
use sha2::{Digest, Sha256};
use waddle_server::{
    db::{Database, DatabaseConfig, DatabaseDriver, MigrationRunner},
    ingress_substrate::{PostgresIngressSubstrate, ALIAS_RETENTION},
};
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey, NormalizedTarget, SemanticDigest};
use waddle_xmpp_core::xep0359::{OriginId, StanzaId};

#[cfg(feature = "clustering")]
use ingress_shadow_support::ShadowFixture;

#[tokio::test]
async fn internal_message_association_and_digest_are_stable() {
    let Some(fixture) = Fixture::open("stability").await else {
        return;
    };
    let key = MessageKey::new();
    let digest = digest(7);
    let mut tx = fixture.store.begin().await.expect("begin alias insert");
    let result = fixture
        .store
        .resolve_and_record_alias(
            &mut tx,
            &bare("romeo@example.com"),
            &NormalizedTarget::Absent,
            &OriginId::new("opaque-origin"),
            &digest,
            || key,
        )
        .await
        .expect("record alias");
    fixture
        .store
        .terminalize_message(&mut tx, key, Utc::now())
        .await
        .expect("terminalize message");
    tx.commit().await.expect("commit stable association");
    assert!(matches!(
        result,
        waddle_xmpp::ingress::AliasResolution::Aliased(
            waddle_xmpp::ingress::AliasOutcome::Inserted(inserted)
        ) if inserted == key
    ));

    // The association survives terminalization: a later resolution still
    // lands on the same canonical key with the identical stored digest.
    let mut tx = fixture.store.begin().await.expect("begin re-resolution");
    let resolved = fixture
        .store
        .resolve_and_record_alias(
            &mut tx,
            &bare("romeo@example.com"),
            &NormalizedTarget::Absent,
            &OriginId::new("opaque-origin"),
            &digest,
            MessageKey::new,
        )
        .await
        .expect("re-resolve alias");
    tx.commit().await.expect("commit re-resolution");
    assert!(matches!(
        resolved,
        waddle_xmpp::ingress::AliasResolution::Aliased(
            waddle_xmpp::ingress::AliasOutcome::Existing(existing)
        ) if existing == key
    ));
    assert_eq!(fixture.message_digest(key).await, digest);
    fixture.close().await;
}

#[tokio::test]
async fn retention_is_non_cascading_and_child_foreign_keys_are_no_action() {
    let Some(fixture) = Fixture::open("retention").await else {
        return;
    };
    let expired = fixture.record_message(digest(1)).await;
    let retained = fixture.record_message(digest(2)).await;
    let past_cutoff = Utc::now() - ALIAS_RETENTION - Duration::seconds(1);
    let mut tx = fixture.store.begin().await.expect("begin terminalize");
    fixture
        .store
        .terminalize_message(&mut tx, expired, past_cutoff)
        .await
        .expect("terminalize expired message");
    tx.commit().await.expect("commit terminalization");
    assert_eq!(
        fixture
            .store
            .gc_expired_aliases(Utc::now())
            .await
            .expect("run GC")
            .deleted_messages,
        1
    );
    assert!(
        fixture.message_exists(retained).await,
        "unrelated live message survives GC"
    );

    let fk_count: i64 = sqlx::query_scalar(&format!(
        "SELECT COUNT(*) FROM pg_constraint WHERE connamespace = '{}'::regnamespace AND contype = 'f' AND conrelid::regclass::text IN ('{}.ingress_origin_aliases', '{}.ingress_sm_refs', '{}.ingress_deliveries') AND confdeltype = 'a'",
        fixture.schema, fixture.schema, fixture.schema, fixture.schema
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("read foreign-key delete actions");
    assert_eq!(fk_count, 3, "every ingress child FK is NO ACTION");
    fixture.close().await;
}

#[tokio::test]
async fn archive_authoritative_effect_intents_bind_the_archive_stanza_id() {
    let Some(fixture) = Fixture::open("archive_intent").await else {
        return;
    };
    let key = fixture.record_message(digest(3)).await;
    let archive = bare("romeo@example.com");
    let intent = IngressEffectIntent::ArchiveAuthoritative {
        archive: archive.clone(),
        stanza_id: StanzaId::new("archive-sid", Jid::from(archive.clone())),
        by: archive.clone(),
    };
    let encoded = intent.encode_v1().expect("encode archive effect intent");

    let conn = fixture.db.guard().await.expect("database guard");
    conn.execute(
        "INSERT INTO ingress_effect_intents (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) VALUES (?::uuid, 0::numeric, ?, ?, 1, ?)",
        waddle_server::db_params![
            key.to_storage().to_string(),
            i64::from(encoded.kind),
            semantic_identity_hash(&intent),
            encoded.payload.clone(),
        ],
    )
    .await
    .expect("insert archive effect intent");
    let mut rows = conn
        .query(
            "SELECT kind::int, payload FROM ingress_effect_intents WHERE message_key = ?::uuid AND effect_ordinal = 0::numeric",
            waddle_server::db_params![key.to_storage().to_string()],
        )
        .await
        .expect("read archive effect intent");
    let row = rows
        .next()
        .await
        .expect("read intent row")
        .expect("intent row");
    let kind: i64 = row.get(0).expect("decode intent kind");
    let payload: Vec<u8> = row.get(1).expect("decode intent payload");
    let decoded = IngressEffectIntent::decode_v1(
        i32::try_from(kind).expect("intent kind fits in i32"),
        &payload,
    )
    .expect("decode persisted archive effect intent");
    assert_eq!(decoded, intent);
    fixture.close().await;
}

/// XEP-0313 §5.1.3 and §6.3 require the archive to assign the authoritative
/// UID while XEP-0359's stanza-id is merely the replay binding. The shadow
/// pipeline must therefore persist the captured archive stanza-id exactly on
/// the authoritative effect row for the handled message.
#[cfg(feature = "clustering")]
#[tokio::test]
async fn shadow_pipeline_binds_archive_authoritative_intent_to_archive_stanza_id() {
    let Some(fixture) = ShadowFixture::open("xep0313_pipeline").await else {
        return;
    };
    let archive = fixture.principal.bare_jid().clone();
    let intent = IngressEffectIntent::ArchiveAuthoritative {
        archive: archive.clone(),
        stanza_id: StanzaId::new("mam-archive-uid-1", Jid::from(archive.clone())),
        by: archive,
    };

    fixture
        .enqueue(fixture.submission_with_intents(
            1,
            Some("mam-origin"),
            "archive authoritative body",
            vec![intent.clone()],
        ))
        .await;
    fixture.wait_for_frontier(1).await;

    let message_key = fixture
        .message_key_for_ordinal(1)
        .await
        .expect("accepted handled ordinal should bind a canonical message");
    let intents = effect_intents_for_message(&fixture, message_key).await;

    assert_eq!(
        intents,
        vec![intent],
        "the persisted authoritative archive intent must retain the archive stanza-id verbatim"
    );

    fixture.close().await;
}

#[cfg(feature = "clustering")]
async fn effect_intents_for_message(
    fixture: &ShadowFixture,
    message_key: MessageKey,
) -> Vec<IngressEffectIntent> {
    let conn = fixture.db.guard().await.expect("database connection");
    let mut rows = conn
        .query(
            "SELECT kind::int, payload FROM ingress_effect_intents WHERE message_key = ?::uuid ORDER BY effect_ordinal",
            waddle_server::db_params![message_key.to_storage().to_string()],
        )
        .await
        .expect("read effect intents");
    let mut intents = Vec::new();
    while let Some(row) = rows.next().await.expect("iterate effect intents") {
        let kind: i64 = row.get(0).expect("decode effect kind");
        let payload: Vec<u8> = row.get(1).expect("decode effect payload");
        intents.push(
            IngressEffectIntent::decode_v1(
                i32::try_from(kind).expect("effect kind fits in i32"),
                &payload,
            )
            .expect("decode effect intent"),
        );
    }
    intents
}

fn digest(byte: u8) -> SemanticDigest {
    SemanticDigest::from_storage(1, [byte; 32]).expect("valid fixture digest")
}

fn bare(value: &str) -> jid::BareJid {
    value.parse().expect("valid fixture bare JID")
}

fn semantic_identity_hash(intent: &IngressEffectIntent) -> Vec<u8> {
    let identity = intent.semantic_key().storage_identity();
    Sha256::digest(identity.as_bytes()).to_vec()
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
            eprintln!("skipping: WADDLE_TEST_POSTGRES_URL not set (XEP-0313 schema)");
            return None;
        };
        let schema = format!(
            "waddle_test_xep0313_{name}_{}",
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
            "xep0313-schema-test",
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

    async fn record_message(&self, digest: SemanticDigest) -> MessageKey {
        let key = MessageKey::new();
        let mut tx = self.store.begin().await.expect("begin message insert");
        self.store
            .record_message(&mut tx, key, &digest)
            .await
            .expect("record message");
        tx.commit().await.expect("commit message insert");
        key
    }

    async fn message_digest(&self, key: MessageKey) -> SemanticDigest {
        let conn = self.db.guard().await.expect("database guard");
        let mut rows = conn
            .query(
                "SELECT digest_version, digest FROM ingress_messages WHERE message_key = ?::uuid",
                waddle_server::db_params![key.to_storage().to_string()],
            )
            .await
            .expect("read message digest");
        let row = rows
            .next()
            .await
            .expect("read digest row")
            .expect("message row");
        let version: i64 = row.get(0).expect("decode version");
        let bytes: Vec<u8> = row.get(1).expect("decode digest");
        SemanticDigest::from_storage(
            u8::try_from(version).expect("valid digest version"),
            bytes.try_into().expect("32 byte digest"),
        )
        .expect("valid stored digest")
    }

    async fn message_exists(&self, key: MessageKey) -> bool {
        let conn = self.db.guard().await.expect("database guard");
        let mut rows = conn
            .query(
                "SELECT 1 FROM ingress_messages WHERE message_key = ?::uuid",
                waddle_server::db_params![key.to_storage().to_string()],
            )
            .await
            .expect("read message");
        rows.next().await.expect("read message row").is_some()
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
