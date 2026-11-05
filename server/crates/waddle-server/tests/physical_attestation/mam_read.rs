//! Exact-handle, restricted-role MAM reads through the dark adapter.

use super::{
    attested_pool, install, test_url, url_with_schema, Database, DatabaseConfig, DatabaseDriver,
    Duration, Fixture, PgConnection, PgRoleName, PgSchemaName, CATALOG_GRANT_LOCK,
};
use sqlx::Connection;
use waddle_server::mam_runtime::{
    DarkMamReadError, DarkMamReader, DatabaseMamStorage, DatabaseMamStorageError,
};
use waddle_xmpp::mam::{
    MamArchiveKind, MamQuery, MamSchemaVerificationError, MamStorage, MamStorageError,
    SqlxMamStorage, StoreOutcome,
};
use waddle_xmpp_core::mam::ArchivedMessage;
use waddle_xmpp_core::xep0359::StanzaId;

#[cfg(feature = "clustering")]
use waddle_server::clustering::claims::initialize_fresh_claims_owner;
#[cfg(feature = "clustering")]
use waddle_xmpp::muc::RoomClaimFenceContext;
#[cfg(feature = "clustering")]
use waddle_xmpp::ownership::{ClaimEpoch, Entity, EntityType, NodeIdentity};

#[tokio::test]
async fn mam_reader_uses_attested_main_database_and_rejects_drift() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let owner: String = sqlx::query_scalar("SELECT current_user::text")
        .fetch_one(&fixture.admin)
        .await
        .expect("fixture owner");
    let owner_role = PgRoleName::new(owner.clone()).expect("valid owner role");
    let unattested = Database::from_config(
        "unattested MAM",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            url_with_schema(&fixture.base_url, &fixture.target),
        ),
    )
    .await
    .expect("owner database");
    assert!(matches!(
        DarkMamReader::attach(&unattested, owner_role.clone()).await,
        Err(DarkMamReadError::AttestedMainDatabaseRequired)
    ));
    let sqlite = Database::in_memory("unsupported MAM SQLite")
        .await
        .expect("SQLite database");
    assert!(matches!(
        DarkMamReader::attach(&sqlite, owner_role.clone()).await,
        Err(DarkMamReadError::AttestedMainDatabaseRequired)
    ));

    let attestation = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install attestor");
    let pool = waddle_server::test_support::database_pool_from_attested_postgres_pool(
        "MAM main and ingress",
        attested_pool(
            fixture.runtime_options.clone(),
            attestation,
            2,
            Duration::from_secs(8),
        )
        .expect("attested pool"),
        1,
        1,
    )
    .await
    .expect("pool lanes");
    let ingress = pool.ingress();
    assert!(matches!(
        DarkMamReader::attach(&ingress, owner_role.clone()).await,
        Err(DarkMamReadError::AttestedMainDatabaseRequired)
    ));
    assert!(DarkMamReader::attach(pool.global(), owner_role.clone())
        .await
        .is_err());
    let missing: bool = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1) IS NULL")
        .bind(format!("{}.mam_messages", fixture.target))
        .fetch_one(&fixture.admin)
        .await
        .expect("schema remains absent");
    assert!(missing, "attachment must not create MAM tables");

    let owner_connection = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("owner connection");
    let mut owner_connection = SqlxMamStorage::bootstrap_postgres_schema_on_owned_connection(
        owner_connection,
        &fixture.target,
        &owner,
    )
    .await
    .expect("owner prepares MAM");
    assert!(
        DarkMamReader::attach(pool.global(), owner_role.clone())
            .await
            .is_err(),
        "reader without SELECT must not attach"
    );
    for table in ["mam_messages", "mam_archive_sequences"] {
        sqlx::query(&format!(
            "GRANT SELECT, INSERT ON {}.{table} TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&mut owner_connection)
        .await
        .expect("grant restricted writer profile");
    }
    sqlx::query(&format!(
        "GRANT UPDATE (next_seq) ON {}.mam_archive_sequences TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&mut owner_connection)
    .await
    .expect("grant restricted counter update");
    sqlx::query(&format!(
        "INSERT INTO {}.mam_archive_sequences (archive_jid, next_seq) VALUES \
         ('room@example.test', 3), ('other@example.test', 1)",
        fixture.target
    ))
    .execute(&mut owner_connection)
    .await
    .expect("seed counters");
    for (id, archive, ordinal, body) in [
        ("z-first", "room@example.test", 1_i64, "first"),
        ("a-second", "room@example.test", 2_i64, "second"),
        ("m-third", "room@example.test", 3_i64, "third"),
        ("other", "other@example.test", 1_i64, "other"),
    ] {
        sqlx::query(&format!(
            "INSERT INTO {}.mam_messages (id, room_jid, timestamp, from_jid, to_jid, body, message_type, archive_seq) \
             VALUES ($1, $2, '2024-01-01T00:00:00Z', 'alice@example.test/device', $2, $3, 'chat', $4)",
            fixture.target
        ))
        .bind(id)
        .bind(archive)
        .bind(body)
        .bind(ordinal)
        .execute(&mut owner_connection)
        .await
        .expect("seed message");
    }
    let archive: jid::BareJid = "room@example.test".parse().expect("archive JID");
    assert!(matches!(
        SqlxMamStorage::query_on_postgres_connection_in_schema(
            &mut owner_connection,
            &fixture.target,
            &archive,
            MamArchiveKind::Room,
            &MamQuery::default(),
        )
        .await,
        Err(MamStorageError::Database(_))
    ));
    let mut shadow_tx = owner_connection.begin().await.expect("shadow transaction");
    sqlx::query(&format!(
        "CREATE TEMP TABLE mam_messages AS SELECT * FROM {}.mam_messages WHERE id = 'z-first'",
        fixture.target
    ))
    .execute(&mut *shadow_tx)
    .await
    .expect("create misleading temporary table");
    let shadow_result = SqlxMamStorage::query_on_postgres_connection_in_schema(
        &mut shadow_tx,
        &fixture.target,
        &archive,
        MamArchiveKind::Room,
        &MamQuery {
            ids: vec!["m-third".to_owned()],
            filter_after_id: Some("z-first".to_owned()),
            after_id: Some("a-second".to_owned()),
            ..Default::default()
        },
    )
    .await
    .expect("all query phases use the qualified target");
    assert_eq!(shadow_result.count, Some(1));
    assert_eq!(shadow_result.messages[0].id, "m-third");
    shadow_tx.rollback().await.expect("discard temporary table");
    owner_connection
        .close()
        .await
        .expect("close owner connection");
    let reader = DarkMamReader::attach(pool.global(), owner_role.clone())
        .await
        .expect("attach owner-prepared archive");
    let held_main = pool.global().begin().await.expect("hold main lane");
    let held_ingress = ingress.begin().await.expect("reserved ingress lane");
    held_ingress.rollback().await.expect("release ingress lane");
    assert!(
        tokio::time::timeout(
            Duration::from_millis(100),
            reader.query(&archive, MamArchiveKind::Room, &MamQuery::default()),
        )
        .await
        .is_err(),
        "MAM reads must share main-lane admission"
    );
    held_main.rollback().await.expect("release main lane");
    let page = reader
        .query(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                max: Some(2),
                ..Default::default()
            },
        )
        .await
        .expect("first page");
    assert_eq!(
        page.messages
            .iter()
            .map(|row| row.id.as_str())
            .collect::<Vec<_>>(),
        ["z-first", "a-second"]
    );
    assert_eq!(page.count, Some(3));
    assert!(!page.complete);
    let tail = reader
        .query(
            &archive,
            MamArchiveKind::Room,
            &MamQuery {
                after_id: Some("a-second".to_owned()),
                ..Default::default()
            },
        )
        .await
        .expect("cursor page");
    assert_eq!(tail.messages.len(), 1);
    assert_eq!(tail.messages[0].id, "m-third");
    for query in [
        MamQuery {
            after_id: Some("other".to_owned()),
            ..Default::default()
        },
        MamQuery {
            ids: vec!["other".to_owned()],
            ..Default::default()
        },
    ] {
        assert!(matches!(
            reader.query(&archive, MamArchiveKind::Room, &query).await,
            Err(DarkMamReadError::Mam(MamStorageError::NotFound(id))) if id == "other"
        ));
    }
    let id = StanzaId::new("z-first", archive.clone().into());
    assert_eq!(
        reader.lookup(&id).await.expect("lookup").expect("row").id,
        "z-first"
    );
    assert!(reader
        .lookup(&StanzaId::new("other", archive.clone().into()))
        .await
        .expect("archive-scoped lookup")
        .is_none());

    for (grant, revoke) in [
        (
            format!(
                "GRANT UPDATE ON {}.mam_messages TO {}",
                fixture.target, fixture.runtime
            ),
            format!(
                "REVOKE UPDATE ON {}.mam_messages FROM {}",
                fixture.target, fixture.runtime
            ),
        ),
        (
            format!(
                "GRANT DELETE ON {}.mam_archive_sequences TO {}",
                fixture.target, fixture.runtime
            ),
            format!(
                "REVOKE DELETE ON {}.mam_archive_sequences FROM {}",
                fixture.target, fixture.runtime
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
                "GRANT SET ON PARAMETER session_replication_role TO {}",
                fixture.runtime
            ),
            format!(
                "REVOKE SET ON PARAMETER session_replication_role FROM {}",
                fixture.runtime
            ),
        ),
    ] {
        sqlx::query(&grant)
            .execute(&fixture.admin)
            .await
            .expect("inject excessive runtime privilege");
        assert!(matches!(
            reader
                .query(&archive, MamArchiveKind::Room, &MamQuery::default())
                .await,
            Err(DarkMamReadError::UnsafePrivileges)
        ));
        sqlx::query(&revoke)
            .execute(&fixture.admin)
            .await
            .expect("revoke excessive runtime privilege");
    }

    sqlx::query(&format!(
        "GRANT SELECT ON {}.mam_messages TO {}",
        fixture.target, fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("inject unsafe grant");
    assert!(matches!(
        reader
            .query(&archive, MamArchiveKind::Room, &MamQuery::default())
            .await,
        Err(DarkMamReadError::UnsafePrivileges)
    ));
    sqlx::query(&format!(
        "REVOKE SELECT ON {}.mam_messages FROM {}",
        fixture.target, fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke injected grant");
    sqlx::query(&format!(
        "GRANT CREATE ON SCHEMA {} TO {}",
        fixture.target, fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("inject schema creation privilege");
    assert!(matches!(
        reader
            .query(&archive, MamArchiveKind::Room, &MamQuery::default())
            .await,
        Err(DarkMamReadError::UnsafePrivileges)
    ));
    sqlx::query(&format!(
        "REVOKE CREATE ON SCHEMA {} FROM {}",
        fixture.target, fixture.outsider
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke injected schema grant");
    sqlx::query(&format!(
        "GRANT {} TO {}",
        fixture.runtime, fixture.outsider
    ))
    .execute(&fixture.cluster_admin)
    .await
    .expect("inject runtime role membership");
    assert!(matches!(
        reader
            .query(&archive, MamArchiveKind::Room, &MamQuery::default())
            .await,
        Err(DarkMamReadError::UnsafePrivileges)
    ));
    sqlx::query(&format!(
        "REVOKE {} FROM {}",
        fixture.runtime, fixture.outsider
    ))
    .execute(&fixture.cluster_admin)
    .await
    .expect("revoke injected role membership");
    sqlx::query(&format!(
        "ALTER TABLE {}.mam_messages ADD COLUMN unexpected text",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("inject schema drift");
    assert!(matches!(
        reader
            .query(&archive, MamArchiveKind::Room, &MamQuery::default())
            .await,
        Err(DarkMamReadError::Schema(
            MamSchemaVerificationError::ColumnDrift
        ))
    ));
    sqlx::query(&format!(
        "ALTER TABLE {}.mam_messages DROP COLUMN unexpected",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore schema");
    assert_eq!(
        reader
            .query(&archive, MamArchiveKind::Room, &MamQuery::default())
            .await
            .expect("read after repair")
            .messages
            .len(),
        3
    );
    drop(reader);
    drop(ingress);
    drop(pool);
    drop(unattested);
    drop(sqlite);
    fixture.cleanup().await;
}

#[tokio::test]
async fn database_mam_storage_uses_one_attested_main_slot_and_rejects_unsafe_grants() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let cluster_url = url.clone();
    let fixture = Fixture::new(url).await;
    // The attestation definer cannot own the target schema: ownership would
    // confer CREATE and make the restricted physical installer reject it.
    let mam_owner = format!("mam_owner_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE ROLE {mam_owner} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"))
        .execute(&fixture.cluster_admin)
        .await
        .expect("provision separate MAM owner");
    let owner_role = PgRoleName::new(mam_owner.clone()).expect("valid MAM owner role");
    let schema = PgSchemaName(fixture.target.clone());

    sqlx::query(&format!(
        "ALTER SCHEMA {} OWNER TO {}",
        fixture.target, mam_owner
    ))
    .execute(&fixture.admin)
    .await
    .expect("assign isolated schema to non-login owner");
    sqlx::query(&format!(
        "GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {mam_owner}"
    ))
    .execute(&fixture.admin)
    .await
    .expect("allow owner to verify physical identity");
    let mut owner_connection = PgConnection::connect_with(&fixture.admin_options)
        .await
        .expect("connect provisioning role");
    sqlx::query(&format!("SET ROLE {mam_owner}"))
        .execute(&mut owner_connection)
        .await
        .expect("assume non-login schema owner");
    owner_connection = SqlxMamStorage::bootstrap_postgres_schema_on_owned_connection(
        owner_connection,
        &fixture.target,
        &mam_owner,
    )
    .await
    .expect("owner prepares MAM schema");

    #[cfg(feature = "clustering")]
    {
        sqlx::query(&format!(
            "CREATE TABLE {}.xmpp_occupancy_authority (full_jid TEXT PRIMARY KEY, generation TEXT NOT NULL)",
            fixture.target
        ))
        .execute(&mut owner_connection)
        .await
        .expect("prepare exact occupancy table");
        let identity = fixture
            .lineage
            .postgres_identity
            .as_ref()
            .expect("PostgreSQL fixture identity")
            .clone();
        let mut owner_url = url::Url::parse(&fixture.base_url).expect("owner URL");
        let retained: Vec<_> = owner_url
            .query_pairs()
            .filter(|(key, _)| key != "options")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        owner_url
            .query_pairs_mut()
            .clear()
            .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
            .append_pair(
                "options",
                &format!(
                    "-c role={} -c search_path={},public",
                    mam_owner, fixture.target
                ),
            );
        let mut owner_config = DatabaseConfig::new(DatabaseDriver::Postgres, owner_url.to_string());
        owner_config.pool_size = 1;
        let owner_database = Database::from_config("MAM claim owner", &owner_config)
            .await
            .expect("open non-login claim owner lane");
        initialize_fresh_claims_owner(&owner_database, &identity)
            .await
            .expect("prepare exact claim schema");
        drop(owner_database);
    }

    for statement in [
        format!(
            "GRANT SELECT, INSERT ON {}.mam_messages TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT UPDATE (body, stanza_xml, thread_id, parent_thread_id, reply_to_id, reply_to_jid, rich_payload) ON {}.mam_messages TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT SELECT, INSERT ON {}.mam_archive_sequences TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT UPDATE (next_seq) ON {}.mam_archive_sequences TO {}",
            fixture.target, fixture.runtime
        ),
    ] {
        sqlx::query(&statement)
            .execute(&mut owner_connection)
            .await
            .expect("grant exact MAM runtime columns");
    }
    #[cfg(feature = "clustering")]
    for statement in [
        format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.clustering_claims TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT SELECT, INSERT, UPDATE ON {}.xmpp_occupancy_authority TO {}",
            fixture.target, fixture.runtime
        ),
        format!(
            "GRANT SELECT, USAGE ON SEQUENCE {}.clustering_claim_epoch_seq TO {}",
            fixture.target, fixture.runtime
        ),
    ] {
        sqlx::query(&statement)
            .execute(&mut owner_connection)
            .await
            .expect("grant exact main claim runtime profile");
    }

    let attestation = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install attestor");
    let pool = waddle_server::test_support::database_pool_from_attested_postgres_pool(
        "MAM writer main and ingress lanes",
        attested_pool(
            fixture.runtime_options.clone(),
            attestation,
            2,
            Duration::from_secs(8),
        )
        .expect("attested main and ingress pool"),
        1,
        1,
    )
    .await
    .expect("separate one-slot main and ingress lanes");
    let database = pool.global().clone();

    let owner_handoff =
        waddle_server::server::MamOwnerClaimHandoff::new(owner_role.clone(), schema.clone())
            .expect("typed MAM owner handoff");
    let inherited =
        waddle_server::server::ResolvedXmppDatabaseUrl::InheritedGlobal(fixture.base_url.clone());
    let clustering = cfg!(feature = "clustering");
    assert_eq!(
        waddle_server::test_support::select_mam_storage_for_test(
            &inherited,
            Some(&owner_handoff),
            clustering,
            clustering,
            &database,
        )
        .await
        .expect("select MAM from exact global main handle"),
        (true, Some(waddle_server::db::lineage::DurableStore::Global)),
        "attested MAM selection must alias the already-attested global boundary"
    );

    // The provenance rejection happens before a main-lane checkout. Use the
    // same URL bytes as the inherited case to distinguish provenance from
    // URL equality.
    let held_main = database.begin().await.expect("hold global main slot");
    for selection in [
        waddle_server::server::ResolvedXmppDatabaseUrl::Explicit(fixture.base_url.clone()),
        waddle_server::server::ResolvedXmppDatabaseUrl::EphemeralFallback,
    ] {
        let error = tokio::time::timeout(
            Duration::from_millis(150),
            waddle_server::test_support::select_mam_storage_for_test(
                &selection,
                Some(&owner_handoff),
                clustering,
                clustering,
                &database,
            ),
        )
        .await
        .expect("unsupported provenance must fail before main checkout")
        .expect_err("attested MAM requires inherited global provenance");
        assert!(error
            .to_string()
            .contains("must inherit WADDLE_DATABASE_URL"));
    }
    held_main
        .rollback()
        .await
        .expect("release global main slot");

    let ingress_database = pool.ingress();
    assert!(
        waddle_server::test_support::select_mam_storage_for_test(
            &inherited,
            Some(&owner_handoff),
            clustering,
            clustering,
            &ingress_database,
        )
        .await
        .is_err(),
        "MAM must reject the reserved ingress handle"
    );

    sqlx::query(&format!(
        "GRANT UPDATE ON {}.mam_messages TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&mut owner_connection)
    .await
    .expect("inject unsafe table-wide message update");
    assert!(matches!(
        DatabaseMamStorage::attach(&database, owner_role.clone(), schema.clone()).await,
        Err(DatabaseMamStorageError::UnsafeMamPrivileges)
    ));
    sqlx::query(&format!(
        "REVOKE UPDATE ON {}.mam_messages FROM {}",
        fixture.target, fixture.runtime
    ))
    .execute(&mut owner_connection)
    .await
    .expect("revoke unsafe table-wide message update");
    // PostgreSQL also clears column UPDATE ACLs with a table-wide REVOKE.
    sqlx::query(&format!(
        "GRANT UPDATE (body, stanza_xml, thread_id, parent_thread_id, reply_to_id, reply_to_jid, rich_payload) ON {}.mam_messages TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&mut owner_connection)
    .await
    .expect("restore exact tombstone columns after broad revoke");

    let ingress_hold = ingress_database
        .begin()
        .await
        .expect("occupy reserved ingress slot");
    let storage = DatabaseMamStorage::attach(&database, owner_role, schema)
        .await
        .expect("attach exact MAM writer");
    let archive: jid::BareJid = "room@example.test".parse().expect("archive JID");
    let message = ArchivedMessage {
        id: "one-slot-message".to_owned(),
        body: Some("stored through the main transaction".to_owned()),
        ..ArchivedMessage::for_test(
            "alice@example.test/device".parse().expect("sender JID"),
            archive.clone().into(),
        )
    };
    assert!(matches!(
        tokio::time::timeout(
            Duration::from_secs(3),
            storage.store_message(&archive, &message)
        )
        .await
        .expect("one-slot MAM write does not reacquire a pool connection")
        .expect("store through attested main slot"),
        StoreOutcome::Stored { .. }
    ));
    let loaded = tokio::time::timeout(Duration::from_secs(3), storage.get_message(&message.id))
        .await
        .expect("one-slot MAM read does not reacquire a pool connection")
        .expect("load through attested main slot")
        .expect("stored message exists");
    assert_eq!(
        loaded.body.as_deref(),
        Some("stored through the main transaction")
    );
    let page = tokio::time::timeout(
        Duration::from_secs(3),
        storage.query_messages(&archive, MamArchiveKind::Room, &MamQuery::default()),
    )
    .await
    .expect("one-slot query does not reacquire a pool connection")
    .expect("query through attested main slot");
    assert_eq!(page.messages.len(), 1);
    ingress_hold
        .rollback()
        .await
        .expect("release reserved ingress slot");

    #[cfg(feature = "clustering")]
    {
        let entity = Entity::new(EntityType::RoomActor, archive.to_string());
        let owner = NodeIdentity::new("mam-test-node", "mam-test-epoch");
        let encoded_entity = entity.to_string();
        let claim_epoch: i64 = sqlx::query_scalar(&format!(
            "INSERT INTO {}.clustering_claims (entity, entity_type, node_id, node_epoch) \
             VALUES ($1, $2, $3, $4) RETURNING claim_epoch",
            fixture.target
        ))
        .bind(encoded_entity)
        .bind(EntityType::RoomActor.as_db_str())
        .bind(&owner.node_id)
        .bind(&owner.node_epoch)
        .fetch_one(&mut owner_connection)
        .await
        .expect("seed exact room claim");
        let fence =
            RoomClaimFenceContext::new(entity.clone(), owner.clone(), ClaimEpoch(claim_epoch));
        let fenced_message = ArchivedMessage {
            id: "fenced-one-slot-message".to_owned(),
            ..ArchivedMessage::for_test(
                "alice@example.test/device".parse().expect("sender JID"),
                archive.clone().into(),
            )
        };
        assert!(matches!(
            tokio::time::timeout(
                Duration::from_secs(3),
                storage.store_message_fenced(&archive, &fenced_message, &fence)
            )
            .await
            .expect("one-slot fenced write stays on pinned transaction")
            .expect("fenced MAM write"),
            StoreOutcome::Stored { .. }
        ));
        let stale_fence = RoomClaimFenceContext::new(entity, owner, ClaimEpoch(claim_epoch + 1));
        let rejected_message = ArchivedMessage {
            id: "stale-one-slot-message".to_owned(),
            ..ArchivedMessage::for_test(
                "alice@example.test/device".parse().expect("sender JID"),
                archive.clone().into(),
            )
        };
        assert!(matches!(
            storage
                .store_message_fenced(&archive, &rejected_message, &stale_fence)
                .await,
            Err(MamStorageError::NotOwner { .. })
        ));
        assert!(storage
            .get_message(&rejected_message.id)
            .await
            .expect("read after stale fence")
            .is_none());
    }

    owner_connection
        .close()
        .await
        .expect("close owner provisioning connection");
    drop(storage);
    drop(ingress_database);
    drop(database);
    pool.global_actor().kill();
    pool.global_actor().wait_for_shutdown().await;
    drop(pool);
    fixture.cleanup().await;
    let cluster_admin = sqlx::PgPool::connect(&cluster_url)
        .await
        .expect("reconnect cluster administrator for owner cleanup");
    sqlx::query(&format!("DROP ROLE {mam_owner}"))
        .execute(&cluster_admin)
        .await
        .expect("drop separate MAM owner");
    cluster_admin.close().await;
}
