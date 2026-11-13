//! Dedicated owner-session MAM bootstrap characterization.

use super::Fixture;
use futures::FutureExt;
use sqlx::{Connection, PgConnection};
use std::time::Duration;
use waddle_server::db::{
    bootstrap_dark_postgres_mam_owner, DarkMamOwnerError, DarkMigrationOwnerConfig,
};

const MAM_LOCK: i64 = 0x7761_6464_6d61_6d31;
const LINEAGE_LOCK: i64 = 6_841_445_497_037_937_993;
const LEDGER_LOCK: i64 = 6_841_445_497_037_937_992;

/// The schema baseline builds the MAM archive. Cases that characterise the
/// owner bootstrap's own DDL start from a schema without it, as a MAM schema
/// the bootstrap has not yet prepared.
async fn drop_mam_archive(fixture: &Fixture) {
    sqlx::query(&format!("DROP TABLE {}.mam_messages", fixture.schema))
        .execute(&fixture.admin)
        .await
        .expect("drop the baseline MAM archive");
}

#[tokio::test]
async fn owner_bootstrap_refuses_missing_migration_ledger() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, false).await;
    let outcome = std::panic::AssertUnwindSafe(async {
        fixture.enable_owner_handoff().await;
        assert!(bootstrap_dark_postgres_mam_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &fixture.migrator_role(),
        )
        .await
        .is_err());
        let absent: bool = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1) IS NULL")
            .bind(format!("{}.mam_messages", fixture.schema))
            .fetch_one(&fixture.admin)
            .await
            .expect("inspect MAM absence");
        assert!(absent);
    })
    .catch_unwind()
    .await;
    fixture.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn owner_bootstrap_requires_identity_and_roles_before_ddl() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    drop_mam_archive(&fixture).await;
    let outcome = std::panic::AssertUnwindSafe(async {
        fixture.enable_owner_handoff().await;
        let migrator = fixture.migrator_role();
        let mut wrong = fixture.lineage.clone();
        wrong.deployment_uuid.0 = uuid::Uuid::new_v4();
        let wrong = DarkMigrationOwnerConfig::new(
            wrong,
            waddle_server::db::physical_attestation::PgRoleName::new(&fixture.owner)
                .expect("owner"),
            waddle_server::db::physical_attestation::PgRoleName::new(&fixture.runtime)
                .expect("runtime"),
        )
        .expect("wrong config");
        assert!(
            bootstrap_dark_postgres_mam_owner(&fixture.migrator_options, &wrong, &migrator)
                .await
                .is_err()
        );
        let mut malformed = fixture.lineage.clone();
        malformed
            .postgres_identity
            .as_mut()
            .expect("PostgreSQL identity")
            .schema
            .name
            .0 = "invalid\"schema".to_owned();
        let malformed = DarkMigrationOwnerConfig::new(
            malformed,
            waddle_server::db::physical_attestation::PgRoleName::new(&fixture.owner)
                .expect("owner"),
            waddle_server::db::physical_attestation::PgRoleName::new(&fixture.runtime)
                .expect("runtime"),
        )
        .expect("malformed schema config");
        assert!(
            bootstrap_dark_postgres_mam_owner(&fixture.migrator_options, &malformed, &migrator,)
                .await
                .is_err(),
            "malformed schema must be rejected before SQL"
        );
        assert!(bootstrap_dark_postgres_mam_owner(
            &fixture.runtime_options,
            &fixture.config(),
            &migrator
        )
        .await
        .is_err());
        sqlx::query(&format!(
            "GRANT TEMP ON DATABASE {} TO {}",
            fixture.database, fixture.migrator
        ))
        .execute(&fixture.admin)
        .await
        .expect("grant test TEMP");
        assert!(bootstrap_dark_postgres_mam_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &migrator
        )
        .await
        .is_err());
        sqlx::query(&format!(
            "REVOKE TEMP ON DATABASE {} FROM {}",
            fixture.database, fixture.migrator
        ))
        .execute(&fixture.admin)
        .await
        .expect("revoke test TEMP");
        let absent: bool = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1) IS NULL")
            .bind(format!("{}.mam_messages", fixture.schema))
            .fetch_one(&fixture.admin)
            .await
            .expect("absence");
        assert!(absent);
        sqlx::query(&format!(
            "ALTER TABLE {}.mam_archive_sequences OWNER TO {}",
            fixture.schema, fixture.runtime,
        ))
        .execute(&fixture.admin)
        .await
        .expect("foreign counter owner");
        assert!(bootstrap_dark_postgres_mam_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &migrator,
        )
        .await
        .is_err());
        let absent: (bool, bool) = sqlx::query_as(
            "SELECT pg_catalog.to_regclass($1) IS NULL, pg_catalog.to_regclass($2) IS NULL",
        )
        .bind(format!("{}.mam_messages", fixture.schema))
        .bind(format!("{}.idx_mam_room_timestamp", fixture.schema))
        .fetch_one(&fixture.admin)
        .await
        .expect("no DDL on wrong owner");
        assert_eq!(absent, (true, true));
        sqlx::query(&format!(
            "ALTER TABLE {}.mam_archive_sequences OWNER TO {}",
            fixture.schema, fixture.owner,
        ))
        .execute(&fixture.admin)
        .await
        .expect("restore counter owner");
        sqlx::query(&format!(
            "CREATE TABLE {}.mam_decoy (id text)",
            fixture.schema
        ))
        .execute(&fixture.admin)
        .await
        .expect("decoy table");
        sqlx::query(&format!(
            "CREATE INDEX idx_mam_room_id ON {}.mam_decoy(id)",
            fixture.schema
        ))
        .execute(&fixture.admin)
        .await
        .expect("colliding index");
        let error = bootstrap_dark_postgres_mam_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &migrator,
        )
        .await
        .expect_err("pre-existing index collision");
        assert!(matches!(
            error,
            DarkMamOwnerError::PreDdl(
                waddle_xmpp::mam::MamSchemaBootstrapError::PreexistingObjectConflict
            )
        ));
        let absent: bool = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1) IS NULL")
            .bind(format!("{}.mam_messages", fixture.schema))
            .fetch_one(&fixture.admin)
            .await
            .expect("no DDL on collision");
        assert!(absent);
        sqlx::query(&format!("DROP TABLE {}.mam_decoy", fixture.schema))
            .execute(&fixture.admin)
            .await
            .expect("remove decoy");
        for _ in 0..2 {
            bootstrap_dark_postgres_mam_owner(
                &fixture.migrator_options,
                &fixture.config(),
                &migrator,
            )
            .await
            .expect("owner bootstrap");
        }
        let owner: String = sqlx::query_scalar(
            "SELECT pg_catalog.pg_get_userbyid(c.relowner)::text FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid=c.relnamespace \
             WHERE n.nspname=$1 AND c.relname='mam_messages'",
        )
        .bind(&fixture.schema)
        .fetch_one(&fixture.admin)
        .await
        .expect("relation owner");
        assert_eq!(owner, fixture.owner);
    })
    .catch_unwind()
    .await;
    fixture.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn owner_preserves_tighter_session_timeouts() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    let outcome = std::panic::AssertUnwindSafe(async {
        fixture.enable_owner_handoff().await;
        sqlx::query(&format!(
            "CREATE FUNCTION {}.check_mam_timeouts() RETURNS event_trigger LANGUAGE plpgsql AS $$ \
             BEGIN IF (SELECT setting::bigint FROM pg_catalog.pg_settings WHERE name='lock_timeout') <> 500 \
                 OR (SELECT setting::bigint FROM pg_catalog.pg_settings WHERE name='statement_timeout') <> 1000 \
                 OR (SELECT setting::bigint FROM pg_catalog.pg_settings WHERE name='idle_session_timeout') <> 2000 \
                 OR (SELECT setting::bigint FROM pg_catalog.pg_settings WHERE name='idle_in_transaction_session_timeout') <> 4000 \
                 THEN RAISE EXCEPTION 'operator timeout was relaxed'; END IF; END $$", fixture.schema,
        )).execute(&fixture.admin).await.expect("timeout check function");
        sqlx::query(&format!(
            "CREATE EVENT TRIGGER fixture_mam_timeouts ON ddl_command_end \
             WHEN TAG IN ('CREATE INDEX') EXECUTE FUNCTION {}.check_mam_timeouts()", fixture.schema,
        )).execute(&fixture.admin).await.expect("timeout check trigger");
        let options = fixture.migrator_options.clone().options([
            ("lock_timeout", "500ms"),
            ("statement_timeout", "1s"),
            ("idle_session_timeout", "2s"),
            ("idle_in_transaction_session_timeout", "4s"),
        ]);
        bootstrap_dark_postgres_mam_owner(&options, &fixture.config(), &fixture.migrator_role())
            .await.expect("bootstrap with tighter inherited timeouts");
        sqlx::query("DROP EVENT TRIGGER fixture_mam_timeouts")
            .execute(&fixture.admin).await.expect("remove timeout trigger");
    }).catch_unwind().await;
    fixture.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn cancelling_mam_lock_wait_releases_session_locks() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    drop_mam_archive(&fixture).await;
    let outcome = std::panic::AssertUnwindSafe(async {
        fixture.enable_owner_handoff().await;
        let mut holder = PgConnection::connect_with(&fixture.admin_options)
            .await
            .expect("holder");
        sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
            .bind(MAM_LOCK)
            .execute(&mut holder)
            .await
            .expect("hold MAM lock");
        let name = format!("mam_owner_wait_{}", uuid::Uuid::new_v4().simple());
        let options = fixture.migrator_options.clone().application_name(&name);
        let config = fixture.config();
        let migrator = fixture.migrator_role();
        let task = tokio::spawn(async move {
            bootstrap_dark_postgres_mam_owner(&options, &config, &migrator).await
        });
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let waiting: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity \
                     WHERE application_name=$1 AND wait_event_type='Lock')",
                )
                .bind(&name)
                .fetch_one(&fixture.admin)
                .await
                .expect("owner wait");
                if waiting {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("owner waiting on MAM lock");
        task.abort();
        assert!(task.await.expect_err("owner cancelled").is_cancelled());
        let unlocked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock($1)")
            .bind(MAM_LOCK)
            .fetch_one(&mut holder)
            .await
            .expect("release blocker");
        assert!(unlocked);
        tokio::time::timeout(Duration::from_secs(3), async {
            for key in [LINEAGE_LOCK, LEDGER_LOCK, MAM_LOCK] {
                loop {
                    let acquired: bool =
                        sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_lock($1)")
                            .bind(key)
                            .fetch_one(&mut holder)
                            .await
                            .expect("probe lock");
                    if acquired {
                        let released: bool =
                            sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock($1)")
                                .bind(key)
                                .fetch_one(&mut holder)
                                .await
                                .expect("release probe");
                        assert!(released);
                        break;
                    }
                    tokio::task::yield_now().await;
                }
            }
        })
        .await
        .expect("backend released its locks");
        let absent: bool = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1) IS NULL")
            .bind(format!("{}.mam_messages", fixture.schema))
            .fetch_one(&fixture.admin)
            .await
            .expect("absence");
        assert!(absent);
        holder.close().await.expect("close holder");
    })
    .catch_unwind()
    .await;
    fixture.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn post_dispatch_failure_reports_partial_and_keeps_committed_work() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    let outcome = std::panic::AssertUnwindSafe(async {
        fixture.enable_owner_handoff().await;
        sqlx::query(&format!(
            "CREATE FUNCTION {}.fail_mam_index() RETURNS event_trigger LANGUAGE plpgsql AS $$ \
             BEGIN RAISE EXCEPTION 'fixture index interruption'; END $$",
            fixture.schema,
        ))
        .execute(&fixture.admin)
        .await
        .expect("fixture trigger function");
        sqlx::query(&format!(
            "CREATE EVENT TRIGGER fixture_interrupt_mam ON ddl_command_end \
             WHEN TAG IN ('CREATE INDEX') EXECUTE FUNCTION {}.fail_mam_index()",
            fixture.schema,
        ))
        .execute(&fixture.admin)
        .await
        .expect("fixture event trigger");
        let error = bootstrap_dark_postgres_mam_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &fixture.migrator_role(),
        )
        .await
        .expect_err("interrupt MAM index DDL");
        assert!(matches!(error, DarkMamOwnerError::PartialOrUnknown));
        let committed: bool = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1) IS NOT NULL")
            .bind(format!("{}.mam_messages", fixture.schema))
            .fetch_one(&fixture.admin)
            .await
            .expect("partial schema");
        assert!(
            committed,
            "an earlier autocommit statement remains committed"
        );
        sqlx::query("DROP EVENT TRIGGER fixture_interrupt_mam")
            .execute(&fixture.admin)
            .await
            .expect("remove fixture trigger");
    })
    .catch_unwind()
    .await;
    fixture.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}

#[tokio::test]
async fn cancelling_dispatched_ddl_requires_fence_then_reconciliation() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url, true).await;
    drop_mam_archive(&fixture).await;
    let outcome = std::panic::AssertUnwindSafe(async {
        fixture.enable_owner_handoff().await;
        const TEST_LOCK: i64 = 5_874_099_219_333_001_447;
        sqlx::query(&format!(
            "CREATE FUNCTION {}.block_mam_index() RETURNS event_trigger LANGUAGE plpgsql AS $$ \
             BEGIN PERFORM pg_catalog.pg_advisory_xact_lock({TEST_LOCK}); END $$",
            fixture.schema,
        ))
        .execute(&fixture.admin)
        .await
        .expect("blocking trigger function");
        sqlx::query(&format!(
            "CREATE EVENT TRIGGER fixture_block_mam ON ddl_command_end \
             WHEN TAG IN ('CREATE INDEX') EXECUTE FUNCTION {}.block_mam_index()",
            fixture.schema,
        ))
        .execute(&fixture.admin)
        .await
        .expect("blocking event trigger");
        let mut holder = PgConnection::connect_with(&fixture.admin_options)
            .await
            .expect("holder");
        sqlx::query("SELECT pg_catalog.pg_advisory_lock($1)")
            .bind(TEST_LOCK)
            .execute(&mut holder)
            .await
            .expect("hold test lock");
        let holder_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
            .fetch_one(&mut holder)
            .await
            .expect("holder pid");
        let name = format!("mam_owner_ddl_{}", uuid::Uuid::new_v4().simple());
        let options = fixture.migrator_options.clone().application_name(&name);
        let config = fixture.config();
        let migrator = fixture.migrator_role();
        let task = tokio::spawn(async move {
            bootstrap_dark_postgres_mam_owner(&options, &config, &migrator).await
        });
        let backend = super::wait_for_application(&fixture.admin, &name, holder_pid).await;
        let query: String =
            sqlx::query_scalar("SELECT query FROM pg_catalog.pg_stat_activity WHERE pid=$1")
                .bind(backend)
                .fetch_one(&fixture.admin)
                .await
                .expect("blocked DDL query");
        assert!(
            query.contains("CREATE INDEX"),
            "owner must be blocked inside dispatched DDL"
        );
        let table_committed: bool =
            sqlx::query_scalar("SELECT pg_catalog.to_regclass($1) IS NOT NULL")
                .bind(format!("{}.mam_messages", fixture.schema))
                .fetch_one(&fixture.admin)
                .await
                .expect("earlier table DDL");
        assert!(
            table_committed,
            "CREATE TABLE committed before the blocked index"
        );
        task.abort();
        assert!(task.await.expect_err("owner cancelled").is_cancelled());
        // Cancellation itself is not fence evidence. Terminate the exact
        // observed backend (or observe it already gone) before retrying.
        let _: bool = sqlx::query_scalar("SELECT pg_catalog.pg_terminate_backend($1)")
            .bind(backend)
            .fetch_one(&fixture.admin)
            .await
            .expect("request backend fence");
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let alive: bool = sqlx::query_scalar(
                    "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1)",
                )
                .bind(backend)
                .fetch_one(&fixture.admin)
                .await
                .expect("observe backend exit");
                if !alive {
                    break;
                }
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("backend fenced");
        let unlocked: bool = sqlx::query_scalar("SELECT pg_catalog.pg_advisory_unlock($1)")
            .bind(TEST_LOCK)
            .fetch_one(&mut holder)
            .await
            .expect("release test lock");
        assert!(unlocked);
        sqlx::query("DROP EVENT TRIGGER fixture_block_mam")
            .execute(&fixture.admin)
            .await
            .expect("remove blocking trigger");
        let index_absent: bool = sqlx::query_scalar("SELECT pg_catalog.to_regclass($1) IS NULL")
            .bind(format!("{}.idx_mam_room_timestamp", fixture.schema))
            .fetch_one(&fixture.admin)
            .await
            .expect("inspect blocked index");
        assert!(index_absent, "fenced index statement did not commit");
        bootstrap_dark_postgres_mam_owner(
            &fixture.migrator_options,
            &fixture.config(),
            &fixture.migrator_role(),
        )
        .await
        .expect("reconcile fenced partial bootstrap");
        holder.close().await.expect("close holder");
    })
    .catch_unwind()
    .await;
    fixture.cleanup().await;
    if let Err(panic) = outcome {
        std::panic::resume_unwind(panic);
    }
}
