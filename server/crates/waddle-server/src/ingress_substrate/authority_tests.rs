use super::*;
use crate::db::{DatabaseConfig, MigrationRunner};

struct Fixture {
    db: Database,
    postgres: Option<(sqlx::PgPool, String)>,
}

impl Fixture {
    async fn open(driver: DatabaseDriver) -> Option<Self> {
        let (db, postgres) = match driver {
            DatabaseDriver::Sqlite => (
                Database::in_memory("ingress-authority")
                    .await
                    .expect("open SQLite"),
                None,
            ),
            DatabaseDriver::Postgres => {
                let Ok(database_url) = std::env::var("WADDLE_TEST_POSTGRES_URL") else {
                    eprintln!(
                        "skipping PostgreSQL authority test: WADDLE_TEST_POSTGRES_URL not set"
                    );
                    return None;
                };
                let admin = sqlx::PgPool::connect(&database_url)
                    .await
                    .expect("connect admin");
                let schema = format!("authority_{}", Uuid::new_v4().simple());
                sqlx::query(&format!("CREATE SCHEMA {schema}"))
                    .execute(&admin)
                    .await
                    .expect("create schema");
                let mut url = url::Url::parse(&database_url).expect("database URL");
                let retained: Vec<_> = url
                    .query_pairs()
                    .filter(|(key, _)| key != "options")
                    .map(|(key, value)| (key.into_owned(), value.into_owned()))
                    .collect();
                url.query_pairs_mut()
                    .clear()
                    .extend_pairs(retained)
                    .append_pair("options", &format!("-c search_path={schema}"));
                let config = DatabaseConfig::new(driver, url.to_string());
                let db = Database::from_config("ingress-authority", &config)
                    .await
                    .expect("open PostgreSQL");
                (db, Some((admin, schema)))
            }
        };
        MigrationRunner::single()
            .run(&db)
            .await
            .expect("migrate authority fixture");
        Some(Self { db, postgres })
    }

    async fn close(self) {
        drop(self.db);
        if let Some((admin, schema)) = self.postgres {
            sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
                .execute(&admin)
                .await
                .expect("drop schema");
        }
    }
}

fn digest() -> SemanticDigest {
    SemanticDigest::from_storage(1, [1; 32]).expect("digest")
}

fn typed_envelope(body: &str) -> MessageEnvelope {
    let mut message = xmpp_parsers::message::Message::new(Some(
        "juliet@example.com"
            .parse::<jid::Jid>()
            .expect("recipient jid"),
    ));
    message.type_ = xmpp_parsers::message::MessageType::Chat;
    message.id = Some(xmpp_parsers::message::Id("envelope-id".to_string()));
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_string());
    MessageEnvelope::new(message).expect("typed envelope")
}

async fn insert_stream(tx: &mut Transaction<'_>, id: SmIngressId) {
    const POSTGRES: &str =
        "INSERT INTO ingress_sm_streams (sm_ingress_id, stream_id) VALUES (?::uuid, ?)";
    const SQLITE: &str = "INSERT INTO ingress_sm_streams (sm_ingress_id, stream_id) VALUES (?, ?)";
    tx.execute(
        dialect_sql(tx.driver(), POSTGRES, SQLITE),
        crate::db_params![id.to_storage().to_string(), "authority-stream"],
    )
    .await
    .expect("insert stream");
}

async fn insert_intent(tx: &mut Transaction<'_>, key: MessageKey, hash: [u8; 32]) {
    const POSTGRES: &str = "INSERT INTO ingress_effect_intents (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) VALUES (?::uuid, 0, 1, ?, 1, ?)";
    const SQLITE: &str = "INSERT INTO ingress_effect_intents (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) VALUES (?, '0', 1, ?, 1, ?)";
    tx.execute(
        dialect_sql(tx.driver(), POSTGRES, SQLITE),
        crate::db_params![key.to_storage().to_string(), hash.to_vec(), vec![1u8]],
    )
    .await
    .expect("insert storage fixture intent");
}

async fn malformed_stored_envelope_fails_typed(driver: DatabaseDriver) {
    let Some(fixture) = Fixture::open(driver).await else {
        return;
    };
    let key = MessageKey::new();
    let mut tx = fixture.db.begin_immediate().await.expect("begin");
    record_message(&mut tx, key, &digest(), None)
        .await
        .expect("record row");
    const POSTGRES: &str = "UPDATE ingress_messages SET envelope_version = 1, envelope = ? WHERE message_key = ?::uuid";
    const SQLITE: &str =
        "UPDATE ingress_messages SET envelope_version = 1, envelope = ? WHERE message_key = ?";
    tx.execute(
        dialect_sql(tx.driver(), POSTGRES, SQLITE),
        crate::db_params![vec![0u8, 128, 255], key.to_storage().to_string()],
    )
    .await
    .expect("poison stored envelope");
    assert!(matches!(
        load_envelope(&mut tx, key).await,
        Err(IngressSubstrateError::InvalidStoredEnvelope)
    ));
    tx.commit().await.expect("close");
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_malformed_stored_envelope_fails_typed() {
    malformed_stored_envelope_fails_typed(DatabaseDriver::Sqlite).await;
}

#[tokio::test]
async fn postgres_malformed_stored_envelope_fails_typed() {
    malformed_stored_envelope_fails_typed(DatabaseDriver::Postgres).await;
}

async fn envelope_roundtrip(driver: DatabaseDriver) {
    let Some(fixture) = Fixture::open(driver).await else {
        return;
    };
    let key = MessageKey::new();
    let empty_key = MessageKey::new();
    let envelope = typed_envelope("round trip body");
    let mut tx = fixture.db.begin_immediate().await.expect("begin");
    assert_eq!(
        load_envelope(&mut tx, key).await.expect("missing envelope"),
        None
    );
    record_message(&mut tx, empty_key, &digest(), None)
        .await
        .expect("record without envelope");
    assert_eq!(
        load_envelope(&mut tx, empty_key)
            .await
            .expect("absent envelope"),
        None
    );
    record_message(&mut tx, key, &digest(), Some(&envelope))
        .await
        .expect("record envelope");
    tx.commit().await.expect("commit");
    let mut tx = fixture.db.begin_immediate().await.expect("read committed");
    assert_eq!(
        load_envelope(&mut tx, key).await.expect("load envelope"),
        Some(envelope)
    );
    tx.commit().await.expect("close read");
    fixture.close().await;
}

async fn wire_binding_unique(driver: DatabaseDriver) {
    let Some(fixture) = Fixture::open(driver).await else {
        return;
    };
    let key = MessageKey::new();
    let id = SmIngressId::new();
    let h = WireHandledCount::from_storage(u32::MAX);
    let mut tx = fixture.db.begin_immediate().await.expect("begin");
    insert_stream(&mut tx, id).await;
    record_message(&mut tx, key, &digest(), None)
        .await
        .expect("record message");
    assert_eq!(
        lookup_wire_binding(&mut tx, id, h)
            .await
            .expect("missing binding"),
        None
    );
    assert_eq!(
        insert_sm_ref(&mut tx, id, IngressOrdinal::FIRST, h, key)
            .await
            .expect("bind wire"),
        MessageWriteOutcome::Recorded
    );
    tx.commit().await.expect("commit binding");
    let mut tx = fixture.db.begin_immediate().await.expect("begin lookup");
    assert_eq!(
        lookup_wire_binding(&mut tx, id, h)
            .await
            .expect("lookup binding"),
        Some((key, IngressOrdinal::FIRST))
    );
    assert_eq!(
        insert_sm_ref(&mut tx, id, IngressOrdinal::FIRST, h, key)
            .await
            .expect("retry binding"),
        MessageWriteOutcome::AlreadyRecorded
    );
    let second = IngressOrdinal::from_storage(2).expect("second ordinal");
    assert!(
        insert_sm_ref(&mut tx, id, second, h, key).await.is_err(),
        "a retained wire position cannot bind another ordinal"
    );
    drop(tx);
    fixture.close().await;
}

async fn checkpoint_frontier(driver: DatabaseDriver) {
    let Some(fixture) = Fixture::open(driver).await else {
        return;
    };
    let id = SmIngressId::new();
    let mut tx = fixture.db.begin_immediate().await.expect("begin");
    assert_eq!(
        load_stream_checkpoint(&mut tx, id)
            .await
            .expect("missing stream"),
        None
    );
    insert_stream(&mut tx, id).await;
    assert_eq!(
        load_stream_checkpoint(&mut tx, id)
            .await
            .expect("initial checkpoint"),
        Some(WireHandledCount::from_storage(0))
    );
    assert_eq!(
        advance_frontier(
            &mut tx,
            id,
            IngressOrdinal::FIRST,
            WireHandledCount::from_storage(7)
        )
        .await
        .expect("advance"),
        FrontierOutcome::Advanced
    );
    assert_eq!(
        load_stream_checkpoint(&mut tx, id)
            .await
            .expect("atomic checkpoint"),
        Some(WireHandledCount::from_storage(7))
    );
    assert_eq!(
        advance_frontier(
            &mut tx,
            id,
            IngressOrdinal::from_storage(3).expect("ordinal"),
            WireHandledCount::from_storage(9)
        )
        .await
        .expect("gap"),
        FrontierOutcome::Stale { stored: 1 }
    );
    assert_eq!(
        load_stream_checkpoint(&mut tx, id)
            .await
            .expect("gap preserves checkpoint"),
        Some(WireHandledCount::from_storage(7))
    );
    flush_checkpoint(&mut tx, id, WireHandledCount::from_storage(u32::MAX))
        .await
        .expect("flush");
    tx.commit().await.expect("commit checkpoint");
    let mut tx = fixture.db.begin_immediate().await.expect("read checkpoint");
    assert_eq!(
        load_stream_checkpoint(&mut tx, id)
            .await
            .expect("durable checkpoint"),
        Some(WireHandledCount::from_storage(u32::MAX))
    );
    tx.commit().await.expect("close read");
    fixture.close().await;
}

async fn receipts_and_gc(driver: DatabaseDriver) {
    let Some(fixture) = Fixture::open(driver).await else {
        return;
    };
    let key = MessageKey::new();
    let kind = EffectReceiptKind::from_storage(1);
    let mut tx = fixture.db.begin_immediate().await.expect("begin");
    record_message(&mut tx, key, &digest(), None)
        .await
        .expect("record message");
    assert!(receipts_complete(&mut tx, key).await.expect("no intents"));
    insert_intent(&mut tx, key, [1; 32]).await;
    insert_intent(&mut tx, key, [2; 32]).await;
    assert!(!receipts_complete(&mut tx, key)
        .await
        .expect("missing receipts"));
    record_receipt(&mut tx, key, kind, &[1; 32])
        .await
        .expect("first receipt");
    record_receipt(&mut tx, key, kind, &[1; 32])
        .await
        .expect("idempotent receipt");
    assert!(!receipts_complete(&mut tx, key)
        .await
        .expect("one receipt missing"));
    tx.commit().await.expect("commit intents");
    record_receipt_pooled(&fixture.db, key, kind, &[2; 32])
        .await
        .expect("post-commit receipt");
    let mut tx = fixture.db.begin_immediate().await.expect("terminalize");
    assert!(receipts_complete(&mut tx, key)
        .await
        .expect("complete receipts"));
    let now = Utc::now();
    terminalize_message(&mut tx, key, now - Duration::days(9))
        .await
        .expect("terminalize");
    tx.commit().await.expect("commit terminalization");
    let outcome = gc_expired_aliases(
        &fixture.db,
        now,
        AliasGcBudget {
            deadline: Instant::now() + StdDuration::from_secs(10),
            lock_timeout: StdDuration::from_secs(1),
            statement_timeout: StdDuration::from_secs(2),
            scan_timeout: StdDuration::from_secs(2),
            progress: AliasGcProgress::default(),
        },
    )
    .await
    .expect("collect terminal message");
    assert_eq!(outcome.deleted_messages, 1);
    let connection = fixture.db.guard().await.expect("read cascades");
    for sql in [
        "SELECT count(*) FROM ingress_messages",
        "SELECT count(*) FROM ingress_effect_intents",
        "SELECT count(*) FROM ingress_effect_receipts",
    ] {
        let mut rows = connection.query(sql, ()).await.expect("count rows");
        let count: i64 = rows
            .next()
            .await
            .expect("read count")
            .expect("count row")
            .get(0)
            .expect("decode count");
        assert_eq!(count, 0, "cascade: {sql}");
    }
    drop(connection);
    fixture.close().await;
}

macro_rules! backend_test {
    ($name:ident, $test:ident, $driver:ident) => {
        #[tokio::test]
        async fn $name() {
            $test(DatabaseDriver::$driver).await;
        }
    };
}
backend_test!(sqlite_envelope_roundtrip, envelope_roundtrip, Sqlite);
backend_test!(postgres_envelope_roundtrip, envelope_roundtrip, Postgres);
backend_test!(sqlite_wire_binding_unique, wire_binding_unique, Sqlite);
backend_test!(postgres_wire_binding_unique, wire_binding_unique, Postgres);
backend_test!(sqlite_checkpoint_frontier, checkpoint_frontier, Sqlite);
backend_test!(postgres_checkpoint_frontier, checkpoint_frontier, Postgres);
backend_test!(sqlite_receipts_and_gc_cascade, receipts_and_gc, Sqlite);
backend_test!(postgres_receipts_and_gc_cascade, receipts_and_gc, Postgres);

async fn alias_envelope_completion(driver: DatabaseDriver) {
    let Some(fixture) = Fixture::open(driver).await else {
        return;
    };
    let key = MessageKey::new();
    let sender = "sender@example.com".parse().expect("sender");
    let target = NormalizedTarget::Full("recipient@example.com/device".parse().expect("target"));
    let origin = OriginId::new("authority-origin");
    let mut tx = fixture.db.begin_immediate().await.expect("begin alias");
    resolve_and_record_alias(&mut tx, &sender, &target, &origin, &digest(), || key)
        .await
        .expect("resolve alias");
    assert_eq!(
        load_envelope(&mut tx, key).await.expect("initial envelope"),
        None
    );
    let envelope = typed_envelope("alias body");
    record_message(&mut tx, key, &digest(), Some(&envelope))
        .await
        .expect("complete alias envelope");
    record_message(&mut tx, key, &digest(), Some(&envelope))
        .await
        .expect("retry immutable envelope");
    record_message(&mut tx, key, &digest(), None)
        .await
        .expect("preserve envelope");
    let conflicting = typed_envelope("conflicting body");
    assert!(matches!(
        record_message(&mut tx, key, &digest(), Some(&conflicting)).await,
        Err(IngressSubstrateError::MessageContentConflict)
    ));
    let other_digest = SemanticDigest::from_storage(1, [2; 32]).expect("other digest");
    assert!(matches!(
        record_message(&mut tx, key, &other_digest, None).await,
        Err(IngressSubstrateError::MessageContentConflict)
    ));
    assert_eq!(
        load_envelope(&mut tx, key)
            .await
            .expect("unchanged envelope"),
        Some(envelope)
    );
    let other_origin = OriginId::new("different-alias-same-candidate");
    assert!(
        matches!(
            resolve_and_record_alias(&mut tx, &sender, &target, &other_origin, &digest(), || key)
                .await,
            Err(IngressSubstrateError::MessageContentConflict)
        ),
        "minting an existing key must not expose it to candidate cleanup"
    );
    tx.commit().await.expect("commit completed envelope");
    fixture.close().await;
}
backend_test!(
    sqlite_alias_envelope_completion,
    alias_envelope_completion,
    Sqlite
);
backend_test!(
    postgres_alias_envelope_completion,
    alias_envelope_completion,
    Postgres
);

#[tokio::test]
async fn postgres_alias_lock_serializes_before_terminalization_without_upgrade_deadlock() {
    let Some(fixture) = Fixture::open(DatabaseDriver::Postgres).await else {
        return;
    };
    let sender = "sender@example.com".parse().expect("sender");
    let target = NormalizedTarget::Full("recipient@example.com/device".parse().expect("target"));
    let origin = OriginId::new("locked-origin");
    let key = MessageKey::new();
    let digest = digest();
    let mut seed = fixture
        .db
        .begin_immediate()
        .await
        .expect("seed transaction");
    resolve_and_record_alias(&mut seed, &sender, &target, &origin, &digest, || key)
        .await
        .expect("seed alias");
    seed.commit().await.expect("commit alias");
    let mut first = fixture.db.begin_immediate().await.expect("first writer");
    resolve_and_record_alias(
        &mut first,
        &sender,
        &target,
        &origin,
        &digest,
        MessageKey::new,
    )
    .await
    .expect("lock alias canonical row");
    let mut second = fixture.db.begin_immediate().await.expect("second writer");
    {
        let resolution = resolve_and_record_alias(
            &mut second,
            &sender,
            &target,
            &origin,
            &digest,
            MessageKey::new,
        );
        tokio::pin!(resolution);
        assert!(
            tokio::time::timeout(StdDuration::from_millis(100), &mut resolution)
                .await
                .is_err(),
            "second alias writer must block before acquiring a canonical share lock"
        );
        tokio::time::timeout(
            StdDuration::from_secs(2),
            terminalize_message(&mut first, key, Utc::now()),
        )
        .await
        .expect("first writer terminalizes without lock upgrade deadlock")
        .expect("terminalize first");
        first.commit().await.expect("release canonical lock");
        let result = tokio::time::timeout(StdDuration::from_secs(2), &mut resolution)
            .await
            .expect("second writer resumes")
            .expect("resolve second alias");
        assert_eq!(
            result,
            AliasResolution::Aliased(waddle_xmpp::ingress::AliasOutcome::Existing(key))
        );
    }
    second
        .commit()
        .await
        .expect("commit second alias transaction");
    fixture.close().await;
}

#[tokio::test]
async fn postgres_already_terminal_row_is_unconditionally_locked() {
    let Some(fixture) = Fixture::open(DatabaseDriver::Postgres).await else {
        return;
    };
    let key = MessageKey::new();
    let mut seed = fixture
        .db
        .begin_immediate()
        .await
        .expect("seed transaction");
    record_message(&mut seed, key, &digest(), None)
        .await
        .expect("seed message");
    terminalize_message(&mut seed, key, Utc::now())
        .await
        .expect("seed terminal state");
    seed.commit().await.expect("commit terminal state");
    let mut first = fixture
        .db
        .begin_immediate()
        .await
        .expect("first transaction");
    assert_eq!(
        terminalize_message(&mut first, key, Utc::now())
            .await
            .expect("lock already terminal row"),
        TerminalizeOutcome::AlreadyTerminal
    );
    let mut second = fixture
        .db
        .begin_immediate()
        .await
        .expect("second transaction");
    {
        let terminalize = terminalize_message(&mut second, key, Utc::now());
        tokio::pin!(terminalize);
        assert!(
            tokio::time::timeout(StdDuration::from_millis(100), &mut terminalize)
                .await
                .is_err(),
            "already-terminal path must retain its unconditional canonical lock"
        );
        first.commit().await.expect("release lock");
        assert_eq!(
            tokio::time::timeout(StdDuration::from_secs(2), &mut terminalize)
                .await
                .expect("second resumes")
                .expect("terminalize second"),
            TerminalizeOutcome::AlreadyTerminal
        );
    }
    second
        .commit()
        .await
        .expect("commit second terminal transaction");
    fixture.close().await;
}
