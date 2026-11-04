//! Exact-handle, restricted-role MAM reads through the dark adapter.

use super::{
    attested_pool, install, test_url, url_with_schema, Database, DatabaseConfig, DatabaseDriver,
    Duration, Fixture, PgConnection, PgRoleName, CATALOG_GRANT_LOCK,
};
use sqlx::Connection;
use waddle_server::mam_runtime::{DarkMamReadError, DarkMamReader};
use waddle_xmpp::mam::{
    MamArchiveKind, MamQuery, MamSchemaVerificationError, MamStorageError, SqlxMamStorage,
};
use waddle_xmpp_core::xep0359::StanzaId;

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
