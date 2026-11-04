//! Production inbox startup selection and lineage registration.

use std::sync::Arc;

use super::{
    attested_pool, install, test_url, url_with_schema, Database, DatabaseConfig, DatabaseDriver,
    DatabaseError, Duration, Fixture, CATALOG_GRANT_LOCK,
};
use waddle_server::config::LineageConfig;
use waddle_server::db::lineage::{DeploymentUuid, DurableStore, LineageAction, LineageStatus};
use waddle_server::db::{DatabasePool, PoolConfig};
use waddle_server::inbox::{bootstrap_inbox_owner_schema, InboxRuntimeAttachError};
use waddle_server::server::{AppState, ResolvedXmppDatabaseUrl};
use waddle_server::test_support::{
    create_inbox_startup_for_test, database_from_attested_postgres_pool,
    database_pool_from_attested_postgres_pool, resolve_xmpp_database_url_for_test,
};
use waddle_xmpp::inbox::storage::InboxStorage;

#[test]
fn url_selection_preserves_explicit_and_inherited_provenance() {
    let url = "postgres://example.test/waddle";
    assert_eq!(
        resolve_xmpp_database_url_for_test(Some(url), Some(url)),
        ResolvedXmppDatabaseUrl::Explicit(url.to_owned()),
        "an override equal to the global URL is still explicit"
    );
    assert_eq!(
        resolve_xmpp_database_url_for_test(Some(" \t "), Some(" postgres://example.test/waddle ")),
        ResolvedXmppDatabaseUrl::InheritedGlobal(url.to_owned())
    );
    assert_eq!(
        resolve_xmpp_database_url_for_test(None, Some(" \t ")),
        ResolvedXmppDatabaseUrl::EphemeralFallback
    );
}

#[tokio::test]
async fn sqlite_startup_keeps_owner_initialization_and_ephemeral_lineage() {
    let pool = Arc::new(
        DatabasePool::try_new(DatabaseConfig::default(), PoolConfig)
            .await
            .expect("SQLite global"),
    );
    let global = pool.global();
    let startup =
        create_inbox_startup_for_test(&ResolvedXmppDatabaseUrl::EphemeralFallback, global)
            .await
            .expect("initialize inbox on SQLite global");
    let user = "alice@example.test".parse().expect("user JID");
    assert!(startup
        .storage()
        .list(&user)
        .await
        .expect("inbox read")
        .is_empty());
    let split = ResolvedXmppDatabaseUrl::Explicit("sqlite:///different.db".to_owned());
    assert!(create_inbox_startup_for_test(&split, global).await.is_err());
    let mut state = AppState::new(Arc::clone(&pool));
    state.lineage_registry.take();
    startup
        .register_lineage(&state)
        .await
        .expect("register inbox");
    state.seal_lineage_registry();
    let registry = state.lineage_registry.get().expect("sealed lineage");
    assert_eq!(registry.alias_target_for_test(DurableStore::Inbox), None);
    let report = registry
        .attest(&LineageConfig::default(), true, Duration::from_secs(1))
        .await;
    assert!(report
        .failures()
        .contains(&(DurableStore::Inbox, LineageStatus::ClusteredEphemeral)));
    state.permission_actor.kill();
    state.room_registry.kill();
    pool.global_actor().kill();
}

#[tokio::test]
async fn attested_inbox_factory_and_lineage_use_only_the_shared_main_lane() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let owner = Database::from_config(
        "inbox startup owner",
        &DatabaseConfig::new(
            DatabaseDriver::Postgres,
            url_with_schema(&fixture.base_url, &fixture.target),
        ),
    )
    .await
    .expect("owner connection");
    let config = install(
        &fixture.installer_options,
        fixture.config(),
        fixture.operation_id,
    )
    .await
    .expect("install physical attestor");
    let pool = Arc::new(
        database_pool_from_attested_postgres_pool(
            "inbox startup",
            attested_pool(
                fixture.runtime_options.clone(),
                config.clone(),
                2,
                Duration::from_millis(400),
            )
            .expect("restricted pool"),
            1,
            1,
        )
        .await
        .expect("main and ingress lanes"),
    );
    let runtime = pool.global().clone();
    let inherited = resolve_xmpp_database_url_for_test(None, Some(&fixture.base_url));

    // Selection refusal must precede both a child connection and global checkout.
    let held = runtime.begin().await.expect("hold main lane");
    for selection in [
        resolve_xmpp_database_url_for_test(Some(&fixture.base_url), Some(&fixture.base_url)),
        ResolvedXmppDatabaseUrl::Explicit("this is not a connection URL".to_owned()),
        ResolvedXmppDatabaseUrl::EphemeralFallback,
    ] {
        let error = tokio::time::timeout(
            Duration::from_millis(150),
            create_inbox_startup_for_test(&selection, &runtime),
        )
        .await
        .expect("selection refusal precedes any checkout")
        .err()
        .expect("unsupported selection refuses");
        assert!(error
            .to_string()
            .contains("must inherit WADDLE_DATABASE_URL"));
    }
    held.rollback().await.expect("release main lane");

    let error = create_inbox_startup_for_test(&inherited, &pool.ingress())
        .await
        .err()
        .expect("ingress lane refuses");
    assert!(matches!(
        error.downcast_ref::<InboxRuntimeAttachError>(),
        Some(InboxRuntimeAttachError::MainLaneRequired)
    ));
    let error = create_inbox_startup_for_test(&inherited, &runtime)
        .await
        .err()
        .expect("unprepared schema refuses");
    assert!(matches!(
        error.downcast_ref::<InboxRuntimeAttachError>(),
        Some(InboxRuntimeAttachError::Relation {
            relation: "inbox_entries",
            ..
        })
    ));
    let exists: Option<String> = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::text")
        .bind(format!("{}.inbox_entries", fixture.target))
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect missing inbox");
    assert!(exists.is_none(), "failed attachment must not repair schema");

    bootstrap_inbox_owner_schema(&owner)
        .await
        .expect("owner prepares inbox");
    for table in ["inbox_entries", "groupchat_notification_recovery"] {
        sqlx::query(&format!(
            "GRANT SELECT ON {}.{table} TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("grant restricted runtime reads");
    }
    let error = create_inbox_startup_for_test(&inherited, &runtime)
        .await
        .err()
        .expect("a read-only role cannot run inbox persistence");
    assert!(matches!(
        error.downcast_ref::<InboxRuntimeAttachError>(),
        Some(InboxRuntimeAttachError::UnsafeRuntimePrivileges {
            component: "inbox_entries"
        })
    ));
    sqlx::query(&format!(
        "GRANT INSERT, UPDATE ON {}.inbox_entries TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant inbox writes");
    sqlx::query(&format!(
        "GRANT INSERT, UPDATE, DELETE ON {}.groupchat_notification_recovery TO {}",
        fixture.target, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant recovery writes");
    sqlx::query(&format!(
        "INSERT INTO {}.inbox_entries (user_jid, partner_jid, kind, last_stanza_id, last_updated) VALUES ('alice@example.test', 'bob@example.test', 'direct', 'message-1', 1)",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("seed inbox");
    let startup = create_inbox_startup_for_test(&inherited, &runtime)
        .await
        .expect("attach prepared inbox through global");
    let user = "alice@example.test".parse().expect("user JID");
    let entries = startup.storage().list(&user).await.expect("runtime read");
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].partner.to_string(), "bob@example.test");
    let partner = "bob@example.test".parse().expect("partner JID");
    assert!(startup
        .storage()
        .mark_read(&user, &partner, None)
        .await
        .expect("restricted runtime updates inbox")
        .is_some());

    // Commit owner DDL while attachment waits for its pre-snapshot table
    // lock. The attachment must inspect the new shape, not an earlier RR
    // snapshot fixed by the physical attestor.
    let mut ddl = fixture.admin.begin().await.expect("owner DDL transaction");
    sqlx::query(&format!(
        "ALTER TABLE {}.inbox_entries ALTER COLUMN last_updated DROP NOT NULL",
        fixture.target
    ))
    .execute(&mut *ddl)
    .await
    .expect("hold inbox schema change");
    let pending = {
        let selection = inherited.clone();
        let runtime = runtime.clone();
        tokio::spawn(async move { create_inbox_startup_for_test(&selection, &runtime).await })
    };
    tokio::time::timeout(Duration::from_secs(3), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                 WHERE datname = $1 AND usename = $2 AND wait_event_type = 'Lock' \
                 AND query LIKE '%inbox_entries%')",
            )
            .bind(&fixture.database_name)
            .bind(&fixture.runtime)
            .fetch_one(&fixture.admin)
            .await
            .expect("observe inbox lock wait");
            if waiting {
                break;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .expect("inbox attachment waits before its snapshot");
    ddl.commit().await.expect("commit changed schema");
    let error = match pending.await.expect("join inbox attachment") {
        Ok(_) => panic!("committed schema drift must be rejected"),
        Err(error) => error,
    };
    assert!(matches!(
        error.downcast_ref::<InboxRuntimeAttachError>(),
        Some(InboxRuntimeAttachError::InvalidColumn {
            table: "inbox_entries",
            column: "last_updated",
            ..
        })
    ));
    sqlx::query(&format!(
        "ALTER TABLE {}.inbox_entries ALTER COLUMN last_updated SET NOT NULL",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore inbox schema");

    let held = runtime.begin().await.expect("occupy main capacity");
    let ingress_db = pool.ingress();
    let ingress = tokio::time::timeout(Duration::from_secs(2), ingress_db.begin())
        .await
        .expect("separate ingress admission")
        .expect("ingress checkout");
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        create_inbox_startup_for_test(&inherited, &runtime),
    )
    .await
    .expect("main acquisition has its configured bound")
    .err()
    .expect("busy main lane times out");
    assert!(matches!(
        error.downcast_ref::<InboxRuntimeAttachError>(),
        Some(InboxRuntimeAttachError::Database(DatabaseError::Internal(
            sqlx::Error::PoolTimedOut
        )))
    ));
    held.rollback().await.expect("release main capacity");
    ingress.rollback().await.expect("release ingress capacity");
    drop(ingress_db);
    create_inbox_startup_for_test(&inherited, &runtime)
        .await
        .expect("main capacity released after timeout");

    let strict = database_from_attested_postgres_pool(
        "strict inbox startup",
        attested_pool(
            fixture
                .runtime_options
                .clone()
                .options([("lock_timeout", "200ms"), ("statement_timeout", "0")]),
            config.clone(),
            1,
            Duration::from_secs(3),
        )
        .expect("attested strict pool"),
    );
    let mut blocker = fixture.admin.begin().await.expect("owner lock transaction");
    sqlx::query(&format!(
        "LOCK TABLE {}.inbox_entries IN ACCESS EXCLUSIVE MODE",
        fixture.target
    ))
    .execute(&mut *blocker)
    .await
    .expect("block inbox verification");
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        create_inbox_startup_for_test(&inherited, &strict),
    )
    .await
    .expect("tighter lock timeout preserved")
    .err()
    .expect("blocked relation refuses");
    match error.downcast_ref::<InboxRuntimeAttachError>() {
        Some(InboxRuntimeAttachError::Relation {
            relation: "inbox_entries",
            source: DatabaseError::Internal(sqlx::Error::Database(error)),
        }) => assert_eq!(error.code().as_deref(), Some("55P03")),
        other => panic!("unexpected bounded attachment error: {other:?}"),
    }
    blocker.rollback().await.expect("release owner lock");
    create_inbox_startup_for_test(&inherited, &strict)
        .await
        .expect("attachment recovers after lock timeout");
    drop(strict);

    let mut wrong_identity = fixture.lineage.clone();
    wrong_identity.lineage_uuid = waddle_server::db::lineage::LineageUuid(uuid::Uuid::new_v4());
    let wrong = database_from_attested_postgres_pool(
        "wrong identity inbox startup",
        attested_pool(
            fixture.runtime_options.clone(),
            fixture.with_lineage(wrong_identity),
            1,
            Duration::from_millis(400),
        )
        .expect("pool with wrong expected lineage"),
    );
    assert!(tokio::time::timeout(
        Duration::from_secs(2),
        create_inbox_startup_for_test(&inherited, &wrong),
    )
    .await
    .expect("wrong identity refusal is bounded")
    .is_err());
    drop(wrong);

    // The global owner path remains separate; alias registration must not
    // repeat an adoption even when it names the shared lineage.
    let mut state = AppState::new(Arc::clone(&pool));
    state.lineage_registry.take();
    state.lineage_config = LineageConfig {
        deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
        action: Some(LineageAction::Adopt(vec![fixture.lineage.lineage_uuid])),
    };
    state.register_lineage_database(DurableStore::Global, runtime.clone());
    startup
        .register_lineage(&state)
        .await
        .expect("register alias without adoption");
    assert_eq!(
        state.lineage_adopt_unmatched(),
        vec![fixture.lineage.lineage_uuid]
    );
    state.seal_lineage_registry();
    assert_eq!(
        state
            .lineage_registry
            .get()
            .expect("sealed lineage")
            .alias_target_for_test(DurableStore::Inbox),
        Some(DurableStore::Global)
    );
    let current: String = sqlx::query_scalar(&format!(
        "SELECT lineage_uuid FROM {}._lineage WHERE id = 1",
        fixture.target
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("existing lineage remains");
    assert_eq!(current, fixture.lineage.lineage_uuid.to_string());

    // Restricted runtime cannot recreate this table; alias registration must
    // succeed without owner DDL and leave it absent.
    sqlx::query(&format!("DROP TABLE {}._lineage", fixture.target))
        .execute(&fixture.admin)
        .await
        .expect("remove lineage table after attachment");
    let state_without_lineage = AppState::new(Arc::clone(&pool));
    startup
        .register_lineage(&state_without_lineage)
        .await
        .expect("alias registration performs no owner DDL");
    let exists: Option<String> = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1)::text")
        .bind(format!("{}._lineage", fixture.target))
        .fetch_one(&fixture.admin)
        .await
        .expect("inspect absent lineage");
    assert!(
        exists.is_none(),
        "registration must not create lineage table"
    );
    state.permission_actor.kill();
    state.room_registry.kill();
    state_without_lineage.permission_actor.kill();
    state_without_lineage.room_registry.kill();
    drop(state);
    drop(state_without_lineage);
    drop(startup);
    drop(runtime);
    pool.global_actor().kill();
    pool.global_actor().wait_for_shutdown().await;
    drop(pool);
    drop(owner);
    fixture.cleanup().await;
}

#[tokio::test]
async fn ordinary_postgres_inbox_startup_preserves_colocation_check() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let target_url = url_with_schema(&fixture.base_url, &fixture.target);
    let global = Database::from_config(
        "ordinary inbox startup",
        &DatabaseConfig::new(DatabaseDriver::Postgres, &target_url),
    )
    .await
    .expect("ordinary global");
    let selected = ResolvedXmppDatabaseUrl::Explicit(target_url);
    let startup = create_inbox_startup_for_test(&selected, &global)
        .await
        .expect("ordinary PostgreSQL owner initialization");
    let user = "alice@example.test".parse().expect("user JID");
    assert!(startup
        .storage()
        .list(&user)
        .await
        .expect("inbox read")
        .is_empty());
    let split = format!("inbox_split_{}", uuid::Uuid::new_v4().simple());
    sqlx::query(&format!("CREATE SCHEMA {split}"))
        .execute(&fixture.admin)
        .await
        .expect("split schema");
    let split_url = ResolvedXmppDatabaseUrl::Explicit(url_with_schema(&fixture.base_url, &split));
    assert!(create_inbox_startup_for_test(&split_url, &global)
        .await
        .is_err());
    let count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_catalog.pg_class c JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace WHERE n.nspname = $1",
    )
    .bind(&split)
    .fetch_one(&fixture.admin)
    .await
    .expect("split objects");
    assert_eq!(count, 0, "refused schema must not be initialized");
    drop(startup);
    drop(global);
    fixture.cleanup().await;
}
