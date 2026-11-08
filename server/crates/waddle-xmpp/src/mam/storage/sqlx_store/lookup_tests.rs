use chrono::{DateTime, Utc};
use jid::BareJid;
use sqlx::postgres::PgPoolOptions;
use sqlx::{Connection, PgConnection, SqliteConnection};
use waddle_xmpp_core::mam::{ArchivedMessage, ArchivedRichMessage, ArchivedTombstone, MamQuery};
use waddle_xmpp_core::xep0359::StanzaId;

use super::SqlxMamStorage;
use crate::mam::storage::{MamArchiveKind, MamStorage, MamStorageError, StoreOutcome};

struct Fixture<'a> {
    id: &'a str,
    archive: &'a BareJid,
    timestamp: &'a str,
    body: Option<&'a str>,
    stanza_id: Option<&'a str>,
    origin_id: Option<&'a str>,
    rich: Option<&'a str>,
    thread: Option<&'a str>,
    parent: Option<&'a str>,
    reply_id: Option<&'a str>,
    reply_jid: Option<&'a str>,
    nickname_generation: Option<i64>,
    ordinal: i64,
}

impl<'a> Fixture<'a> {
    fn new(id: &'a str, archive: &'a BareJid, ordinal: i64) -> Self {
        Self {
            id,
            archive,
            timestamp: "2024-02-01T12:00:00Z",
            body: None,
            stanza_id: None,
            origin_id: None,
            rich: None,
            thread: None,
            parent: None,
            reply_id: None,
            reply_jid: None,
            nickname_generation: None,
            ordinal,
        }
    }
}

enum Harness<'a> {
    Sqlite(&'a mut SqliteConnection),
    Postgres {
        conn: &'a mut PgConnection,
        schema: &'a str,
    },
}

impl Harness<'_> {
    async fn insert(&mut self, row: Fixture<'_>) {
        let (table, timestamp) = match self {
            Self::Sqlite(_) => ("mam_messages".to_owned(), "$3"),
            Self::Postgres { schema, .. } => (
                super::postgres_relation(Some(schema), "mam_messages"),
                "$3::text::timestamptz",
            ),
        };
        let sql = format!(
            "INSERT INTO {table} (id, room_jid, timestamp, from_jid, to_jid, body, stanza_id, origin_id, message_type, rich_payload, thread_id, parent_thread_id, reply_to_id, reply_to_jid, nickname_generation, archive_seq) VALUES ($1, $2, {timestamp}, 'alice@example.com/device', $2, $4, $5, $6, 'chat', $7, $8, $9, $10, $11, $12, $13)"
        );
        macro_rules! insert {
            ($conn:expr) => {
                sqlx::query(&sql)
                    .bind(row.id)
                    .bind(row.archive.as_str())
                    .bind(row.timestamp)
                    .bind(row.body)
                    .bind(row.stanza_id)
                    .bind(row.origin_id)
                    .bind(row.rich)
                    .bind(row.thread)
                    .bind(row.parent)
                    .bind(row.reply_id)
                    .bind(row.reply_jid)
                    .bind(row.nickname_generation)
                    .bind(row.ordinal)
                    .execute($conn)
                    .await
                    .expect("insert fixture")
            };
        }
        match self {
            Self::Sqlite(conn) => {
                insert!(&mut **conn);
            }
            Self::Postgres { conn, .. } => {
                insert!(&mut **conn);
            }
        }
    }

    async fn lookup(&mut self, id: &StanzaId) -> Result<Option<ArchivedMessage>, MamStorageError> {
        match self {
            Self::Sqlite(conn) => {
                SqlxMamStorage::lookup_archive_or_stanza_id_on_sqlite_connection(conn, id).await
            }
            Self::Postgres { conn, schema } => {
                SqlxMamStorage::lookup_archive_or_stanza_id_on_postgres_connection_in_schema(
                    conn, schema, id,
                )
                .await
            }
        }
    }
}

async fn lookup_contract(harness: &mut Harness<'_>) {
    let archive: BareJid = "lookup@rooms.example".parse().expect("archive");
    let other: BareJid = "other@rooms.example".parse().expect("other archive");
    let identity = |name: &str| StanzaId::new(name, archive.clone().into());

    let mut first = Fixture::new("shared", &archive, 1);
    first.stanza_id = Some("old-wire");
    first.body = Some("first");
    harness.insert(first).await;

    let mut rich = ArchivedRichMessage::default();
    rich.subjects.insert("en".into(), "topic".into());
    let rich_json = serde_json::to_string(&rich).expect("encode rich fixture");
    let mut newer = Fixture::new("newer", &archive, 2);
    newer.timestamp = "2020-01-01T00:00:00Z";
    newer.body = Some("");
    newer.stanza_id = Some("shared");
    newer.origin_id = Some("origin-only");
    newer.rich = Some(&rich_json);
    newer.thread = Some("thread");
    newer.parent = Some("parent");
    newer.reply_id = Some("reply");
    newer.reply_jid = Some("bob@example.com");
    newer.nickname_generation = Some(7);
    harness.insert(newer).await;

    harness
        .insert(Fixture::new("null-fields", &archive, 3))
        .await;
    let mut broken = Fixture::new("malformed", &archive, 4);
    broken.parent = Some("orphan");
    harness.insert(broken).await;
    let mut invalid_ordinal = Fixture::new("invalid-ordinal", &archive, 0);
    invalid_ordinal.body = Some("bad sequence");
    harness.insert(invalid_ordinal).await;

    // This later ordinal in a different archive must never win the collision.
    let mut collision = Fixture::new("other-row", &other, 99);
    collision.stanza_id = Some("shared");
    harness.insert(collision).await;

    let got = harness
        .lookup(&identity("shared"))
        .await
        .expect("lookup collision")
        .expect("matching row");
    assert_eq!(got.id, "newer");
    assert_eq!(got.ordinal.expect("ordinal").to_storage(), 2);
    assert_eq!(
        got.timestamp,
        DateTime::parse_from_rfc3339("2020-01-01T00:00:00Z")
            .expect("timestamp")
            .with_timezone(&Utc)
    );
    assert_eq!(got.body.as_deref(), Some(""));
    assert_eq!(got.rich, Some(rich));
    assert_eq!(got.nickname_generation, Some(7));
    let thread = got.thread.expect("thread");
    assert_eq!(thread.id.as_str(), "thread");
    assert_eq!(thread.parent.expect("parent").as_str(), "parent");
    let reply = got.reply.expect("reply");
    assert_eq!(reply.id.as_str(), "reply");
    assert_eq!(reply.to.expect("reply to").to_string(), "bob@example.com");
    assert_eq!(got.stanza_id.expect("wire id"), identity("shared"));
    assert_eq!(got.origin_id.expect("origin").as_str(), "origin-only");

    let from_full_jid = StanzaId::new(
        "newer",
        "lookup@rooms.example/device".parse().expect("full JID"),
    );
    assert_eq!(
        harness
            .lookup(&from_full_jid)
            .await
            .expect("lookup by id")
            .expect("id row")
            .id,
        "newer"
    );
    assert_eq!(
        harness
            .lookup(&identity("old-wire"))
            .await
            .expect("old wire")
            .expect("old row")
            .id,
        "shared"
    );
    let null = harness
        .lookup(&identity("null-fields"))
        .await
        .expect("nullable row")
        .expect("present");
    assert_eq!(null.body, None);
    assert_eq!(null.rich, None);
    assert_eq!(null.stanza_id, None);
    assert_eq!(null.thread, None);
    assert_eq!(null.reply, None);
    assert_eq!(null.nickname_generation, None);
    assert!(harness
        .lookup(&identity("origin-only"))
        .await
        .expect("origin miss")
        .is_none());
    assert!(harness
        .lookup(&StanzaId::new("newer", other.clone().into()))
        .await
        .expect("wrong archive miss")
        .is_none());
    assert!(matches!(
        harness.lookup(&identity("malformed")).await,
        Err(MamStorageError::Serialization(_))
    ));
    assert!(matches!(
        harness.lookup(&identity("invalid-ordinal")).await,
        Err(MamStorageError::InvalidOrdinal(_))
    ));
}

#[tokio::test]
async fn sqlite_lookup_uses_pinned_connection_and_preserves_decode_contract() {
    let storage = SqlxMamStorage::open_in_memory().await.expect("schema");
    let pool = storage.sqlite_pool().expect("SQLite pool");
    let mut tx = pool.begin().await.expect("transaction");
    lookup_contract(&mut Harness::Sqlite(&mut tx)).await;
    // A stale connection-local temp table must not shadow the main database.
    sqlx::query("CREATE TEMP TABLE mam_messages AS SELECT * FROM main.mam_messages WHERE 0")
        .execute(&mut *tx)
        .await
        .expect("shadow table");
    let archive: BareJid = "lookup@rooms.example".parse().expect("archive");
    let mut shadow = Fixture::new("shadow", &archive, 100);
    shadow.stanza_id = Some("shared");
    Harness::Sqlite(&mut tx).insert(shadow).await;
    let id = StanzaId::new("shared", archive.clone().into());
    assert_eq!(
        SqlxMamStorage::lookup_archive_or_stanza_id_on_sqlite_connection(&mut tx, &id)
            .await
            .expect("qualified lookup")
            .expect("main row")
            .id,
        "newer"
    );
    tx.rollback().await.expect("rollback");
    assert!(storage
        .get_message_by_archive_or_stanza_id(&archive, "shared")
        .await
        .expect("lookup after rollback")
        .is_none());
}

#[tokio::test]
async fn sqlite_connection_helpers_ignore_temp_archive_shadows() {
    let storage = SqlxMamStorage::open_in_memory().await.expect("schema");
    let pool = storage.sqlite_pool().expect("SQLite pool");
    let mut tx = pool.begin().await.expect("transaction");
    sqlx::query("CREATE TEMP TABLE mam_messages AS SELECT * FROM main.mam_messages WHERE 0")
        .execute(&mut *tx)
        .await
        .expect("temporary message shadow");
    sqlx::query(
        "CREATE TEMP TABLE mam_archive_sequences AS SELECT * FROM main.mam_archive_sequences WHERE 0",
    )
    .execute(&mut *tx)
    .await
    .expect("temporary sequence shadow");

    let archive: BareJid = "main-shadow@rooms.example".parse().expect("archive");
    let message_id = uuid::Uuid::now_v7().to_string();
    let message = ArchivedMessage {
        id: message_id.clone(),
        body: Some("persistent archive row".to_owned()),
        stanza_id: Some(StanzaId::new("main-shadow-wire", archive.clone().into())),
        ..ArchivedMessage::for_test(
            "alice@example.com/device".parse().expect("sender"),
            archive.clone().into(),
        )
    };
    assert!(matches!(
        SqlxMamStorage::store_message_on_sqlite_connection(&mut tx, &archive, &message)
            .await
            .expect("store in main"),
        StoreOutcome::Stored { stanza_id, .. } if stanza_id == message_id
    ));

    let page = SqlxMamStorage::query_on_sqlite_connection(
        &mut tx,
        &archive,
        MamArchiveKind::Room,
        &MamQuery::default(),
    )
    .await
    .expect("query main");
    assert_eq!(page.messages.len(), 1);
    assert_eq!(page.messages[0].id, message_id);
    let cursor_query = MamQuery {
        before_id: Some(message_id.clone()),
        ..Default::default()
    };
    assert!(SqlxMamStorage::query_on_sqlite_connection(
        &mut tx,
        &archive,
        MamArchiveKind::Room,
        &cursor_query,
    )
    .await
    .expect("query cursor in main")
    .messages
    .is_empty());
    assert_eq!(
        SqlxMamStorage::count_messages_on_sqlite_connection(&mut tx, &archive)
            .await
            .expect("count main"),
        1
    );
    assert_eq!(
        SqlxMamStorage::get_message_by_archive_or_stanza_id_on_sqlite_connection(
            &mut tx,
            &archive,
            "main-shadow-wire",
        )
        .await
        .expect("lookup main")
        .expect("persistent message")
        .id,
        message_id
    );
    assert!(SqlxMamStorage::replace_with_tombstone_on_sqlite_connection(
        &mut tx,
        &message_id,
        ArchivedTombstone {
            retraction_id: None,
            stamp: Utc::now(),
            moderation: None,
            sender_scope: None,
        },
    )
    .await
    .expect("tombstone main row"));
    assert!(
        SqlxMamStorage::get_message_on_sqlite_connection(&mut tx, &message_id)
            .await
            .expect("read tombstoned main row")
            .expect("persistent row")
            .body
            .is_none()
    );

    let main_messages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM main.mam_messages")
        .fetch_one(&mut *tx)
        .await
        .expect("main row count");
    let temp_messages: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM temp.mam_messages")
        .fetch_one(&mut *tx)
        .await
        .expect("temporary row count");
    let next_ordinal: i64 =
        sqlx::query_scalar("SELECT next_seq FROM main.mam_archive_sequences WHERE archive_jid = ?")
            .bind(archive.as_str())
            .fetch_one(&mut *tx)
            .await
            .expect("main sequence");
    let temp_sequences: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM temp.mam_archive_sequences")
        .fetch_one(&mut *tx)
        .await
        .expect("temporary sequence count");
    assert_eq!(main_messages, 1);
    assert_eq!(temp_messages, 0);
    assert_eq!(next_ordinal, 1);
    assert_eq!(temp_sequences, 0);

    tx.rollback().await.expect("rollback");
}

#[tokio::test]
async fn sqlite_lookup_does_not_initialize_schema() {
    let mut conn = SqliteConnection::connect("sqlite::memory:")
        .await
        .expect("connection");
    let id = StanzaId::new("missing", "lookup@rooms.example".parse().expect("JID"));
    assert!(matches!(
        SqlxMamStorage::lookup_archive_or_stanza_id_on_sqlite_connection(&mut conn, &id).await,
        Err(MamStorageError::Database(_))
    ));
}

#[tokio::test]
async fn postgres_lookup_uses_attested_schema_and_pinned_connection() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        eprintln!("skipping Postgres MAM lookup test: WADDLE_TEST_POSTGRES_URL is unset");
        return;
    };
    let pool = PgPoolOptions::new()
        .max_connections(1)
        .connect(&url)
        .await
        .expect("pool");
    let mut tx = pool.begin().await.expect("transaction");
    // The quote exercises identifier escaping as well as schema qualification.
    let schema = format!("mam_lookup_\"{}", uuid::Uuid::now_v7().simple());
    let quoted = format!("\"{}\"", schema.replace('"', "\"\""));
    sqlx::query(&format!("CREATE SCHEMA {quoted}"))
        .execute(&mut *tx)
        .await
        .expect("schema");
    let relation = super::postgres_relation(Some(&schema), "mam_messages");
    sqlx::query(&format!(
        "CREATE TABLE {relation} (id TEXT PRIMARY KEY, room_jid TEXT NOT NULL, timestamp TIMESTAMPTZ NOT NULL, from_jid TEXT NOT NULL, to_jid TEXT NOT NULL, body TEXT, stanza_id TEXT, thread_id TEXT, reply_to_id TEXT, reply_to_jid TEXT, origin_id TEXT, message_type TEXT NOT NULL, stanza_xml TEXT, rich_payload TEXT, nickname_generation BIGINT, parent_thread_id TEXT, archive_seq BIGINT NOT NULL)"
    ))
    .execute(&mut *tx)
    .await
    .expect("schema table");
    // An unqualified lookup would see this shadow table instead.
    sqlx::query(&format!("CREATE TEMP TABLE mam_messages (LIKE {relation})"))
        .execute(&mut *tx)
        .await
        .expect("shadow table");
    let archive: BareJid = "lookup@rooms.example".parse().expect("archive");
    let mut shadow = Fixture::new("shadow", &archive, 100);
    shadow.stanza_id = Some("shared");
    Harness::Postgres {
        conn: &mut tx,
        schema: "pg_temp",
    }
    .insert(shadow)
    .await;
    lookup_contract(&mut Harness::Postgres {
        conn: &mut tx,
        schema: &schema,
    })
    .await;
    let id = StanzaId::new("missing", archive.into());
    assert!(matches!(
        SqlxMamStorage::lookup_archive_or_stanza_id_on_postgres_connection_in_schema(
            &mut tx,
            "missing_mam_lookup_schema",
            &id,
        )
        .await,
        Err(MamStorageError::Database(_))
    ));
    tx.rollback().await.expect("rollback schema and fixtures");
}
