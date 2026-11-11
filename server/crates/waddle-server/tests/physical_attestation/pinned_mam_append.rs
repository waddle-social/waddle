//! PG17 witnesses for the conditional MAM shape and origin observation.

use std::time::Duration;

use sqlx::postgres::types::Oid;
use sqlx::{Connection, PgConnection, Row};
use waddle_server::db::physical_attestation::{
    pinned_attested_pool, PinnedMamAppendDiagnostic, PinnedMamAppendError, PinnedMamAppendPins,
    PinnedRuntimeAuthority,
};
use waddle_server::db::{
    DarkMigrationLedgerPins, Database, DatabaseConfig, DatabaseDriver, MigrationRunner,
};
use waddle_xmpp::mam::storage::{BoundedMamOriginError, BoundedMamOriginProbe};
use waddle_xmpp::mam::{
    ArchivedMessage, ArchivedRichMessage, ArchivedRichPayload, ArchivedTombstone, SqlxMamStorage,
};
use waddle_xmpp_core::xep0359::OriginId;
use xmpp_parsers::message::MessageType;

use super::{dark_attestor_fixture, test_url, url_with_schema, Fixture, CATALOG_GRANT_LOCK};

const INDEXES: [&str; 10] = [
    "mam_messages_pkey",
    "idx_mam_room_timestamp",
    "idx_mam_room_sender",
    "idx_mam_room_id",
    "idx_mam_room_thread",
    "idx_mam_room_reply_to",
    "idx_mam_room_origin",
    "idx_mam_room_stanza",
    "idx_mam_room_archive_seq",
    "mam_archive_sequences_pkey",
];

async fn oid(fixture: &Fixture, name: &str) -> u32 {
    let value: Oid = sqlx::query_scalar(
        "SELECT c.oid FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND c.relname=$2",
    )
    .bind(&fixture.target)
    .bind(name)
    .fetch_one(&fixture.admin)
    .await
    .expect("fixture object");
    value.0
}

async fn diagnostic(fixture: &Fixture) -> PinnedMamAppendDiagnostic {
    let (config, attestor) = dark_attestor_fixture(fixture).await;
    let url = url_with_schema(&fixture.base_url, &fixture.target);
    let database = Database::from_config(
        "MAM diagnostic migration",
        &DatabaseConfig::new(DatabaseDriver::Postgres, &url),
    )
    .await
    .expect("migration database");
    MigrationRunner::single()
        .run(&database)
        .await
        .expect("migrate");
    drop(database);
    let mam = SqlxMamStorage::open(&url)
        .await
        .expect("actual MAM bootstrap");
    drop(mam);
    for table in [
        "_migrations",
        "ingress_protocol_epoch",
        "ingress_epoch_guard_manifest",
    ] {
        sqlx::query(&format!(
            "GRANT SELECT ON {}.{} TO {}",
            fixture.target, table, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("grant preflight read");
    }
    for table in ["mam_messages", "mam_archive_sequences"] {
        sqlx::query(&format!(
            "GRANT SELECT, INSERT ON {}.{} TO {}",
            fixture.target, table, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("grant append-profile access");
    }
    sqlx::query(&format!(
        "GRANT UPDATE(next_seq) ON {}.mam_archive_sequences TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant counter update");
    let schema = sqlx::query(
        "SELECT n.oid, n.nspowner, r.oid FROM pg_catalog.pg_namespace n JOIN pg_catalog.pg_roles r ON r.rolname=$2 WHERE n.nspname=$1",
    )
    .bind(&fixture.target)
    .bind(&fixture.runtime)
    .fetch_one(&fixture.admin)
    .await
    .expect("independently record role pins");
    let ledger = DarkMigrationLedgerPins {
        schema_oid: schema.get::<Oid, _>(0).0,
        schema_owner_role_oid: schema.get::<Oid, _>(1).0,
        runtime_role_oid: schema.get::<Oid, _>(2).0,
        ledger_relation_oid: oid(fixture, "_migrations").await,
        ledger_primary_index_oid: oid(fixture, "_migrations_pkey").await,
    };
    let authority = PinnedRuntimeAuthority::new(
        &config,
        attestor,
        ledger,
        oid(fixture, "ingress_protocol_epoch").await,
        oid(fixture, "ingress_protocol_epoch_pkey").await,
        oid(fixture, "ingress_epoch_guard_manifest").await,
    )
    .expect("pinned runtime");
    let mut indexes = [0; 10];
    for (slot, name) in INDEXES.into_iter().enumerate() {
        indexes[slot] = oid(fixture, name).await;
    }
    let pins = PinnedMamAppendPins {
        messages_relation_oid: oid(fixture, "mam_messages").await,
        sequences_relation_oid: oid(fixture, "mam_archive_sequences").await,
        index_oids: indexes,
    };
    let pool = pinned_attested_pool(
        fixture.runtime_options.clone(),
        config,
        authority,
        1,
        Duration::from_secs(5),
    )
    .expect("pinned pool");
    PinnedMamAppendDiagnostic::new(pool, pins).expect("diagnostic")
}

fn incoming(origin: &str) -> ArchivedMessage {
    let mut message = ArchivedMessage::for_test(
        "room@example.test/alice".parse().expect("sender"),
        "room@example.test".parse().expect("archive"),
    );
    message.origin_id = Some(OriginId::new(origin));
    message.message_type = MessageType::Groupchat;
    message.body = Some("message".into());
    message
}

fn request(message: &ArchivedMessage) -> BoundedMamOriginProbe {
    BoundedMamOriginProbe::new(&"room@example.test".parse().expect("archive"), message)
        .expect("bounded request")
}

async fn observe(
    diagnostic: &PinnedMamAppendDiagnostic,
    message: &ArchivedMessage,
) -> Result<Option<waddle_xmpp_core::xep0359::StanzaId>, PinnedMamAppendError> {
    diagnostic
        .start_probe(request(message))
        .expect("start")
        .finish()
        .await
}

async fn waiting_for_relation(fixture: &Fixture, relation: u32) -> i32 {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT pid FROM pg_catalog.pg_locks WHERE locktype='relation' \
                 AND relation=$1 AND mode='AccessShareLock' AND NOT granted LIMIT 1",
            )
            .bind(Oid(relation))
            .fetch_optional(&fixture.admin)
            .await
            .expect("observe relation waiter");
            if let Some(pid) = pid {
                break pid;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("diagnostic reaches relation lock")
}

async fn assert_backend_rolled_back(fixture: &Fixture, pid: i32) {
    let idle: Option<bool> = sqlx::query_scalar(
        "SELECT xact_start IS NULL FROM pg_catalog.pg_stat_activity WHERE pid=$1",
    )
    .bind(pid)
    .fetch_optional(&fixture.admin)
    .await
    .expect("observe diagnostic backend");
    assert_eq!(idle, Some(true), "same backend must acknowledge rollback");
}

fn tombstone(scope: Option<&str>) -> String {
    serde_json::to_string(&ArchivedRichMessage {
        payload: Some(ArchivedRichPayload::Tombstone(ArchivedTombstone {
            retraction_id: None,
            stamp: chrono::Utc::now(),
            moderation: None,
            sender_scope: scope.map(|value| value.parse().expect("scope")),
        })),
        ..ArchivedRichMessage::default()
    })
    .expect("encode tombstone")
}

async fn seed(fixture: &Fixture, id: &str, origin: &str, seq: i64, rich: Option<&str>) {
    sqlx::query(&format!(
        "INSERT INTO {}.mam_messages (id,room_jid,timestamp,from_jid,to_jid,origin_id,message_type,rich_payload,archive_seq) VALUES ($1,'room@example.test',now(),'room@example.test/alice',         'room@example.test',$2,'groupchat',$3,$4)",
        fixture.target
    ))
    .bind(id).bind(origin).bind(rich).bind(seq)
    .execute(&fixture.admin).await.expect("seed message");
    sqlx::query(&format!(
        "INSERT INTO {}.mam_archive_sequences VALUES ('room@example.test',$1) ON CONFLICT (archive_jid) DO UPDATE SET next_seq=GREATEST(mam_archive_sequences.next_seq,EXCLUDED.next_seq)",
        fixture.target
    )).bind(seq).execute(&fixture.admin).await.expect("seed counter");
}

#[tokio::test]
async fn origin_observation_preserves_order_refusals_and_scope() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new_with_c_collation(url).await;
    let diagnostic = diagnostic(&fixture).await;
    let old = tombstone(None);
    seed(&fixture, "old", "ordered", 1, Some(&old)).await;
    seed(&fixture, "new", "ordered", 2, None).await;
    assert_eq!(
        observe(&diagnostic, &incoming("ordered"))
            .await
            .expect("hit")
            .expect("row")
            .id,
        "old"
    );
    assert!(observe(&diagnostic, &incoming("missing"))
        .await
        .expect("miss")
        .is_none());

    let mut subject = incoming("ordered");
    subject.body = None;
    subject.rich = Some(ArchivedRichMessage::default());
    subject
        .rich
        .as_mut()
        .expect("rich")
        .subjects
        .insert(String::new(), "topic".into());
    assert!(observe(&diagnostic, &subject)
        .await
        .expect("subject")
        .is_none());
    let mut chat = incoming("ordered");
    chat.message_type = MessageType::Chat;
    assert!(observe(&diagnostic, &chat).await.expect("chat").is_none());

    let scoped = tombstone(Some("alice@example.test"));
    seed(&fixture, "scoped", "scope", 3, Some(&scoped)).await;
    assert!(observe(&diagnostic, &incoming("scope"))
        .await
        .expect("missing real sender")
        .is_none());

    sqlx::query(&format!(
        "INSERT INTO {}.mam_messages (id,room_jid,timestamp,from_jid,to_jid,origin_id,message_type,rich_payload,archive_seq) SELECT 'bulk-'||n,'room@example.test',now(),'room@example.test/alice',        'room@example.test','bulk','groupchat',CASE WHEN n=1 THEN $1 ELSE NULL END,n+3 FROM pg_catalog.generate_series(1,65) n", fixture.target
    )).bind(&old).execute(&fixture.admin).await.expect("seed overflow");
    assert!(matches!(
        observe(&diagnostic, &incoming("bulk")).await,
        Err(PinnedMamAppendError::Probe(
            BoundedMamOriginError::CandidateOverflow
        ))
    ));

    sqlx::query(&format!(
        "INSERT INTO {}.mam_messages (id,room_jid,timestamp,from_jid,to_jid,origin_id,message_type,rich_payload,archive_seq) VALUES ('huge','room@example.test',now(),'room@example.test/alice',         'room@example.test','huge','groupchat',pg_catalog.repeat('x',17*1024*1024),100)",
        fixture.target
    )).execute(&fixture.admin).await.expect("seed compressed oversized rich payload");
    assert!(matches!(
        observe(&diagnostic, &incoming("huge")).await,
        Err(PinnedMamAppendError::Probe(
            BoundedMamOriginError::RowTooLarge
        ))
    ));

    seed(&fixture, "malformed", "invalid", 101, Some("{")).await;
    assert!(matches!(
        observe(&diagnostic, &incoming("invalid")).await,
        Err(PinnedMamAppendError::Probe(BoundedMamOriginError::Decode))
    ));
    drop(diagnostic);
    fixture.cleanup().await;
}

#[tokio::test]
async fn append_related_shape_and_grant_drift_refuse() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new_with_c_collation(url).await;
    let diagnostic = diagnostic(&fixture).await;
    assert!(observe(&diagnostic, &incoming("missing"))
        .await
        .expect("baseline")
        .is_none());
    for (introduce, restore) in [
        (
            format!("GRANT SELECT ON {}.mam_messages TO PUBLIC", fixture.target),
            format!(
                "REVOKE SELECT ON {}.mam_messages FROM PUBLIC",
                fixture.target
            ),
        ),
        (
            format!(
                "GRANT SELECT ON {}.mam_archive_sequences TO {}",
                fixture.target, fixture.outsider
            ),
            format!(
                "REVOKE SELECT ON {}.mam_archive_sequences FROM {}",
                fixture.target, fixture.outsider
            ),
        ),
        (
            format!(
                "GRANT UPDATE (archive_jid) ON {}.mam_archive_sequences TO {}",
                fixture.target, fixture.runtime
            ),
            format!(
                "REVOKE UPDATE (archive_jid) ON {}.mam_archive_sequences FROM {}",
                fixture.target, fixture.runtime
            ),
        ),
        (
            format!(
                "ALTER TABLE {}.mam_messages ALTER COLUMN message_type SET DEFAULT 'chat'",
                fixture.target
            ),
            format!(
                "ALTER TABLE {}.mam_messages ALTER COLUMN message_type SET DEFAULT 'normal'",
                fixture.target
            ),
        ),
        (
            format!(
                "ALTER TABLE {}.mam_archive_sequences ADD CONSTRAINT surprise CHECK (next_seq>=0)",
                fixture.target
            ),
            format!(
                "ALTER TABLE {}.mam_archive_sequences DROP CONSTRAINT surprise",
                fixture.target
            ),
        ),
        (
            format!(
                "CREATE TRIGGER surprise BEFORE UPDATE ON {}.mam_messages FOR EACH ROW EXECUTE FUNCTION pg_catalog.suppress_redundant_updates_trigger()",
                fixture.target
            ),
            format!("DROP TRIGGER surprise ON {}.mam_messages", fixture.target),
        ),
        (
            format!(
                "CREATE INDEX surprise ON {}.mam_archive_sequences(next_seq)",
                fixture.target
            ),
            format!("DROP INDEX {}.surprise", fixture.target),
        ),
    ] {
        sqlx::query(&introduce)
            .execute(&fixture.admin)
            .await
            .expect("introduce drift");
        assert!(matches!(
            observe(&diagnostic, &incoming("missing")).await,
            Err(PinnedMamAppendError::CatalogDrift)
        ));
        sqlx::query(&restore)
            .execute(&fixture.admin)
            .await
            .expect("restore");
    }
    assert!(observe(&diagnostic, &incoming("missing"))
        .await
        .expect("restored")
        .is_none());
    drop(diagnostic);
    fixture.cleanup().await;
}

#[tokio::test]
async fn stale_and_unknown_database_collation_versions_refuse_before_probe() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    for options in [
        "TEMPLATE template0 LOCALE_PROVIDER builtin BUILTIN_LOCALE 'C' COLLATION_VERSION '__stale__'",
        "TEMPLATE template0 LOCALE_PROVIDER libc LC_COLLATE 'C' LC_CTYPE 'C'",
    ] {
        let fixture = Fixture::new_with_database_options(url.clone(), options).await;
        let diagnostic = diagnostic(&fixture).await;
        assert!(matches!(observe(&diagnostic, &incoming("missing")).await,
            Err(PinnedMamAppendError::UnsafeCollation)));
        drop(diagnostic);
        fixture.cleanup().await;
    }
}

#[tokio::test]
async fn sequence_index_replacement_after_wait_refuses_on_the_original_backend() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new_with_c_collation(url).await;
    let diagnostic = diagnostic(&fixture).await;
    let messages = oid(&fixture, "mam_messages").await;
    let sequences = oid(&fixture, "mam_archive_sequences").await;
    let mut owner = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("catalog mutation backend");
    let mut mutation = owner.begin().await.expect("catalog mutation transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}.mam_archive_sequences IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *mutation)
    .await
    .expect("hold counter heap");

    let job = diagnostic
        .start_probe(request(&incoming("missing")))
        .expect("start observation");
    let pid = waiting_for_relation(&fixture, sequences).await;
    let messages_locked: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 \
         AND relation=$2 AND mode='AccessShareLock' AND granted)",
    )
    .bind(pid)
    .bind(Oid(messages))
    .fetch_one(&fixture.admin)
    .await
    .expect("observe preceding message lock");
    assert!(
        messages_locked,
        "message heap must be locked before counter heap"
    );

    sqlx::query(&format!(
        "ALTER TABLE {}.mam_archive_sequences DROP CONSTRAINT mam_archive_sequences_pkey",
        fixture.target
    ))
    .execute(&mut *mutation)
    .await
    .expect("drop pinned primary index");
    sqlx::query(&format!(
        "ALTER TABLE {}.mam_archive_sequences ADD CONSTRAINT mam_archive_sequences_pkey PRIMARY KEY (archive_jid)",
        fixture.target
    ))
    .execute(&mut *mutation)
    .await
    .expect("replace with same-shape primary index");
    mutation.commit().await.expect("publish replacement");

    assert!(matches!(
        job.finish().await,
        Err(PinnedMamAppendError::CatalogDrift)
    ));
    assert_backend_rolled_back(&fixture, pid).await;
    drop(diagnostic);
    drop(owner);
    fixture.cleanup().await;
}

#[tokio::test]
async fn cancelled_receiver_retains_admission_until_rollback() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new_with_c_collation(url).await;
    let diagnostic = diagnostic(&fixture).await;
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let job = diagnostic
        .start_probe_paused_for_test(request(&incoming("missing")), ready_tx, release_rx)
        .expect("start");
    let pid = ready_rx.await.expect("backend");
    drop(job);
    assert!(matches!(
        diagnostic.start_probe(request(&incoming("missing"))),
        Err(PinnedMamAppendError::Overloaded)
    ));
    let held: bool = sqlx::query_scalar(
        "SELECT pg_catalog.count(*)=2 FROM pg_catalog.pg_locks WHERE pid=$1 AND relation=ANY($2::pg_catalog.oid[]) AND mode='AccessShareLock' AND granted"
    ).bind(pid).bind(vec![Oid(oid(&fixture,"mam_messages").await), Oid(oid(&fixture,"mam_archive_sequences").await)])
        .fetch_one(&fixture.admin).await.expect("inspect locks");
    assert!(held);
    release_tx.send(()).expect("release");
    let next = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match diagnostic.start_probe(request(&incoming("missing"))) {
                Ok(job) => break job,
                Err(PinnedMamAppendError::Overloaded) => {
                    tokio::time::sleep(Duration::from_millis(10)).await
                }
                Err(error) => panic!("unexpected admission: {error}"),
            }
        }
    })
    .await
    .expect("worker cleanup");
    assert!(next.finish().await.expect("next observation").is_none());
    drop(diagnostic);
    fixture.cleanup().await;
}
