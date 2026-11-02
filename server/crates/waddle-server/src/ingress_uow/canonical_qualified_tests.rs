//! Canonical UoW statements must not follow a hostile PostgreSQL search path.

mod acceptance;
mod no_store;
mod post_commit_repositories;
mod principal;
mod sm;

use super::{CanonicalMessageRepository, IngressUnitOfWork, IngressUowTransaction};
use crate::config::LineageConfig;
use crate::db::lineage::{self, AttestedLineage, DeploymentUuid, PgSchemaName};
use crate::db::physical_attestation::{
    attested_pool, install, PgRoleName, PhysicalAttestationConfig,
};
use crate::db::{
    Database, DatabaseConfig, DatabaseDriver, DatabasePool, MigrationRunner, PoolConfig,
};
use crate::ingress_substrate::MessageEnvelope;
use futures::FutureExt;
use sqlx::postgres::{PgConnectOptions, PgPool};
use std::str::FromStr;
use std::time::Duration;
use waddle_xmpp::ingress::{
    AliasOutcome, AliasResolution, MessageKey, NormalizedTarget, SemanticDigest,
};
use waddle_xmpp_core::xep0359::OriginId;
use xmpp_parsers::message::Message;

#[derive(Clone)]
struct Fixture {
    cluster_admin: PgPool,
    admin: PgPool,
    database_name: String,
    target: String,
    definer: String,
    installer: String,
    runtime: String,
    lineage: AttestedLineage,
    config: PhysicalAttestationConfig,
    runtime_options: PgConnectOptions,
    parent_control_acl: Option<String>,
}

impl Fixture {
    async fn open(url: &str) -> Self {
        let cluster_admin = PgPool::connect(url).await.expect("cluster admin");
        let shared: bool = sqlx::query_scalar("SELECT relisshared FROM pg_catalog.pg_class WHERE oid = 'pg_catalog.pg_proc'::pg_catalog.regclass")
            .fetch_one(&cluster_admin).await.expect("catalog scope");
        assert!(!shared, "function privileges must be database-local");
        let parent_control_acl = control_acl(&cluster_admin).await;
        let suffix = uuid::Uuid::new_v4().simple().to_string();
        let database_name = format!("canonical_db_{suffix}");
        let target = format!("canonical_t_{suffix}");
        let trusted = format!("canonical_f_{suffix}");
        let definer = format!("canonical_d_{suffix}");
        let installer = format!("canonical_i_{suffix}");
        let runtime = format!("canonical_r_{suffix}");
        sqlx::query(&format!("CREATE DATABASE {database_name}"))
            .execute(&cluster_admin)
            .await
            .expect("create test database");
        sqlx::query(&format!(
            "REVOKE TEMPORARY ON DATABASE {database_name} FROM PUBLIC"
        ))
        .execute(&cluster_admin)
        .await
        .expect("revoke public temp");
        sqlx::query(&format!("CREATE ROLE {definer} NOLOGIN NOSUPERUSER NOCREATEDB NOCREATEROLE NOREPLICATION NOBYPASSRLS"))
            .execute(&cluster_admin).await.expect("definer role");
        for role in [&installer, &runtime] {
            sqlx::query(&format!("CREATE ROLE {role} LOGIN"))
                .execute(&cluster_admin)
                .await
                .expect("login role");
        }
        sqlx::query(&format!(
            "GRANT {definer} TO {installer} WITH INHERIT FALSE, SET TRUE, ADMIN FALSE"
        ))
        .execute(&cluster_admin)
        .await
        .expect("installer role handoff");
        let database_url = with_database(url, &database_name);
        let admin = PgPool::connect(&database_url)
            .await
            .expect("database admin");
        for statement in [
            "REVOKE ALL ON SCHEMA public FROM PUBLIC".to_owned(),
            format!("CREATE SCHEMA {target}"),
            format!("CREATE SCHEMA {trusted} AUTHORIZATION {definer}"),
            format!("GRANT USAGE ON SCHEMA {target} TO {runtime}"),
        ] {
            sqlx::query(&statement)
                .execute(&admin)
                .await
                .expect("schema setup");
        }
        let boundary_url = with_schema(&database_url, &target);
        let database = Database::from_config(
            "canonical fixture migration",
            &DatabaseConfig::new(DatabaseDriver::Postgres, boundary_url.clone()),
        )
        .await
        .expect("migration database");
        let lineage_config = LineageConfig {
            deployment_uuid: Some(DeploymentUuid(uuid::Uuid::new_v4())),
            action: None,
        };
        let lineage = lineage::enroll(&database, &lineage_config)
            .await
            .expect("lineage");
        MigrationRunner::single()
            .run(&database)
            .await
            .expect("migrations");
        drop(database);
        for statement in [
            "REVOKE EXECUTE ON FUNCTION pg_catalog.pg_control_system() FROM PUBLIC".to_owned(),
            format!("GRANT EXECUTE ON FUNCTION pg_catalog.pg_control_system() TO {definer}"),
            format!("GRANT USAGE ON SCHEMA {target} TO {definer}"),
            format!("GRANT SELECT ON {target}._lineage TO {definer}"),
        ] {
            sqlx::query(&statement)
                .execute(&admin)
                .await
                .expect("attestor grants");
        }
        let config = PhysicalAttestationConfig::new(
            lineage.clone(),
            PgRoleName::new(installer.clone()).expect("installer"),
            PgRoleName::new(runtime.clone()).expect("runtime"),
            PgRoleName::new(definer.clone()).expect("definer"),
            PgSchemaName(trusted),
        )
        .expect("physical identity configuration");
        let options = PgConnectOptions::from_str(&boundary_url).expect("connection options");
        let config = install(
            &options.clone().username(&installer),
            config,
            uuid::Uuid::new_v4(),
        )
        .await
        .expect("install attestor");
        for statement in [
            format!("GRANT SELECT, INSERT, UPDATE, DELETE ON ALL TABLES IN SCHEMA {target} TO {runtime}"),
            format!("GRANT USAGE, SELECT, UPDATE ON ALL SEQUENCES IN SCHEMA {target} TO {runtime}"),
        ] {
            sqlx::query(&statement).execute(&admin).await.expect("runtime grants");
        }
        assert_eq!(
            control_acl(&cluster_admin).await,
            parent_control_acl,
            "fixture must not change the parent database function ACL"
        );
        Self {
            cluster_admin,
            admin,
            database_name,
            target,
            definer,
            installer,
            runtime: runtime.clone(),
            lineage,
            config,
            runtime_options: options.username(&runtime),
            parent_control_acl,
        }
    }

    async fn grant_temp(&self, enabled: bool) {
        let action = if enabled { "GRANT" } else { "REVOKE" };
        let separator = if enabled { "TO" } else { "FROM" };
        sqlx::query(&format!(
            "{action} TEMPORARY ON DATABASE {} {separator} {}",
            self.database_name, self.runtime
        ))
        .execute(&self.admin)
        .await
        .expect("adjust test TEMP privilege");
    }

    async fn seed_shadows(&self, tx: &mut IngressUowTransaction<'_>) {
        self.grant_temp(true).await;
        for statement in [
            "CREATE TEMP TABLE ingress_protocol_epoch (id integer PRIMARY KEY, epoch bigint NOT NULL) ON COMMIT PRESERVE ROWS",
            "INSERT INTO pg_temp.ingress_protocol_epoch VALUES (1, 2)",
            "CREATE TEMP TABLE ingress_messages (message_key uuid PRIMARY KEY, digest_version integer, digest bytea, envelope_version integer, envelope bytea, created_at timestamptz DEFAULT now(), terminal_at timestamptz, retention_eligible_at timestamptz) ON COMMIT PRESERVE ROWS",
            "CREATE TEMP TABLE ingress_origin_aliases (alias_key_hash bytea PRIMARY KEY, sender_bare_jid text, target_kind integer, target_jid text, origin_id text, message_key uuid) ON COMMIT PRESERVE ROWS",
        ] {
            tx.transaction_mut().execute(statement, ()).await.expect("create shadow");
        }
        self.grant_temp(false).await;
        tx.transaction_mut()
            .execute(
                &format!("SET LOCAL search_path = pg_temp, {}", self.target),
                (),
            )
            .await
            .expect("hostile local search path");
    }

    async fn cleanup(self) {
        self.admin.close().await;
        sqlx::query(&format!(
            "DROP DATABASE {} WITH (FORCE)",
            self.database_name
        ))
        .execute(&self.cluster_admin)
        .await
        .expect("drop test database");
        assert_eq!(
            control_acl(&self.cluster_admin).await,
            self.parent_control_acl,
            "dropping the fixture must preserve the parent database function ACL"
        );
        for role in [&self.runtime, &self.installer, &self.definer] {
            sqlx::query(&format!("DROP ROLE IF EXISTS {role}"))
                .execute(&self.cluster_admin)
                .await
                .expect("drop test role");
        }
        self.cluster_admin.close().await;
    }
}

async fn control_acl(pool: &PgPool) -> Option<String> {
    sqlx::query_scalar("SELECT proacl::text FROM pg_catalog.pg_proc WHERE oid = 'pg_catalog.pg_control_system()'::pg_catalog.regprocedure")
        .fetch_one(pool).await.expect("control function ACL")
}

fn with_database(base: &str, name: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL URL");
    let retained: Vec<_> = url
        .query_pairs()
        .filter(|(key, _)| key != "dbname" && key != "options")
        .map(|(key, value)| (key.into_owned(), value.into_owned()))
        .collect();
    url.query_pairs_mut()
        .clear()
        .extend_pairs(retained)
        .append_pair("dbname", name);
    url.to_string()
}

fn with_schema(base: &str, schema: &str) -> String {
    let mut url = url::Url::parse(base).expect("PostgreSQL URL");
    url.query_pairs_mut()
        .append_pair("options", &format!("-c search_path={schema}"));
    url.to_string()
}

fn digest(byte: u8) -> SemanticDigest {
    SemanticDigest::from_storage(1, [byte; 32]).expect("digest")
}

fn envelope(body: &str) -> MessageEnvelope {
    let mut message = Message::new(Some("bob@example.test".parse().expect("target")));
    message.bodies.insert(Default::default(), body.to_owned());
    MessageEnvelope::new(message)
}

async fn count(tx: &mut IngressUowTransaction<'_>, sql: &str) -> i64 {
    let mut rows = tx
        .transaction_mut()
        .query(sql, ())
        .await
        .expect("query count");
    rows.next()
        .await
        .expect("count row")
        .expect("count exists")
        .get(0)
        .expect("integer")
}

async fn resolve(
    tx: &mut IngressUowTransaction<'_>,
    origin: &str,
    digest: &SemanticDigest,
    key: MessageKey,
) -> AliasResolution {
    CanonicalMessageRepository::resolve_and_record_alias(
        tx,
        &"alice@example.test".parse().expect("sender"),
        &NormalizedTarget::Bare("bob@example.test".parse().expect("target")),
        &OriginId::new(origin),
        digest,
        || key,
    )
    .await
    .expect("resolve alias")
}

#[tokio::test]
async fn attested_canonical_statements_ignore_hostile_temporary_shadows() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::open(&url).await;
    let result = std::panic::AssertUnwindSafe(postgres_case(&fixture))
        .catch_unwind()
        .await;
    fixture.cleanup().await;
    if let Err(panic) = result {
        std::panic::resume_unwind(panic);
    }
}

async fn postgres_case(fixture: &Fixture) {
    let pool = attested_pool(
        fixture.runtime_options.clone(),
        fixture.config.clone(),
        2,
        Duration::from_secs(2),
    )
    .expect("attested pool");
    let pool = DatabasePool::from_attested_postgres_pool("canonical fixture", pool, 1, 1)
        .await
        .expect("shared lanes");
    let uow = IngressUnitOfWork::open(
        pool.ingress(),
        LineageConfig {
            deployment_uuid: Some(fixture.lineage.deployment_uuid),
            action: None,
        },
    )
    .expect("UoW");
    let key = MessageKey::new();
    let semantic_digest = digest(7);
    let original = envelope("one");
    let updated = envelope("room canonical");
    let mut tx = uow.begin().await.expect("prepared transaction");
    assert!(tx.receipt_schema().is_some(), "same attested ingress lane");
    let pid = count(&mut tx, "SELECT pg_catalog.pg_backend_pid()::bigint").await;
    fixture.seed_shadows(&mut tx).await;
    let raw_error = crate::ingress_substrate::record_message(
        tx.transaction_mut(),
        MessageKey::new(),
        &semantic_digest,
        None,
    )
    .await
    .expect_err("raw path must observe the hostile temporary epoch");
    assert!(matches!(
        raw_error,
        crate::ingress_substrate::IngressSubstrateError::UnsupportedLiveEpoch
    ));
    assert!(
        matches!(resolve(&mut tx, "origin", &semantic_digest, key).await, AliasResolution::Aliased(AliasOutcome::Inserted(saved)) if saved == key)
    );
    CanonicalMessageRepository::record_message(&mut tx, key, &semantic_digest, Some(&original))
        .await
        .expect("complete alias envelope");
    assert!(CanonicalMessageRepository::lock(&mut tx, key)
        .await
        .expect("lock"));
    assert!(!CanonicalMessageRepository::is_terminal(&mut tx, key)
        .await
        .expect("terminal"));
    assert_eq!(
        CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("load"),
        Some(original)
    );
    let _ = CanonicalMessageRepository::created_at(&mut tx, key)
        .await
        .expect("created at");
    CanonicalMessageRepository::record_room_canonical_envelope(&mut tx, key, &updated)
        .await
        .expect("room envelope");
    assert_eq!(
        CanonicalMessageRepository::load_envelope(&mut tx, key)
            .await
            .expect("load updated"),
        Some(updated)
    );
    tx.transaction_mut()
        .execute(
            &format!(
                "UPDATE {}.ingress_messages SET terminal_at = now() WHERE message_key = ?::uuid",
                fixture.target
            ),
            crate::db_params![key.to_storage().to_string()],
        )
        .await
        .expect("mark terminal");
    assert!(CanonicalMessageRepository::is_terminal(&mut tx, key)
        .await
        .expect("terminal"));
    CanonicalMessageRepository::clear_terminal(&mut tx, key)
        .await
        .expect("clear terminal");
    assert!(!CanonicalMessageRepository::is_terminal(&mut tx, key)
        .await
        .expect("cleared"));
    assert_eq!(
        count(&mut tx, "SELECT count(*) FROM pg_temp.ingress_messages").await,
        0
    );
    assert_eq!(
        count(
            &mut tx,
            "SELECT count(*) FROM pg_temp.ingress_origin_aliases"
        )
        .await,
        0
    );
    assert_eq!(
        count(&mut tx, "SELECT pg_catalog.pg_backend_pid()::bigint").await,
        pid
    );
    tx.commit().await.expect("commit");

    let mut replay = uow.begin().await.expect("replay transaction");
    assert_eq!(
        count(&mut replay, "SELECT pg_catalog.pg_backend_pid()::bigint").await,
        pid,
        "one ingress slot returns the same backend"
    );
    fixture.seed_shadows(&mut replay).await;
    assert!(
        matches!(resolve(&mut replay, "origin", &semantic_digest, MessageKey::new()).await, AliasResolution::Aliased(AliasOutcome::Existing(saved)) if saved == key)
    );
    assert!(
        matches!(resolve(&mut replay, "origin", &digest(8), MessageKey::new()).await, AliasResolution::Aliased(AliasOutcome::Conflict(saved)) if saved.existing == key)
    );
    CanonicalMessageRepository::record_message(&mut replay, key, &semantic_digest, None)
        .await
        .expect("matching digest replay");
    assert_eq!(
        count(&mut replay, "SELECT count(*) FROM pg_temp.ingress_messages").await,
        0
    );
    replay.commit().await.expect("replay commit");

    let mut rollback = uow.begin().await.expect("rollback transaction");
    fixture.seed_shadows(&mut rollback).await;
    assert!(matches!(
        resolve(
            &mut rollback,
            "rolled-back",
            &semantic_digest,
            MessageKey::new()
        )
        .await,
        AliasResolution::Aliased(AliasOutcome::Inserted(_))
    ));
    drop(rollback);
    let mut check = uow.begin().await.expect("post rollback");
    assert_eq!(
        count(
            &mut check,
            &format!(
                "SELECT count(*) FROM {}.ingress_origin_aliases",
                fixture.target
            )
        )
        .await,
        1
    );
    assert_eq!(
        count(
            &mut check,
            &format!("SELECT count(*) FROM {}.ingress_messages", fixture.target)
        )
        .await,
        1
    );
    check.commit().await.expect("check commit");

    // Force the loser through INSERT ... ON CONFLICT and candidate deletion.
    // The main and ingress budgets each have one slot on the same attested pool.
    let main_uow = IngressUnitOfWork::open(
        pool.global().clone(),
        LineageConfig {
            deployment_uuid: Some(fixture.lineage.deployment_uuid),
            action: None,
        },
    )
    .expect("main-lane UoW");
    let mut winner = main_uow.begin().await.expect("winner transaction");
    winner
        .set_local_timeouts(10_000, 10_000)
        .await
        .expect("bound winner race waits");
    let winner_pid = count(&mut winner, "SELECT pg_catalog.pg_backend_pid()::bigint").await;
    let race_key = MessageKey::new();
    assert!(matches!(
        resolve(&mut winner, "race", &semantic_digest, race_key).await,
        AliasResolution::Aliased(AliasOutcome::Inserted(saved)) if saved == race_key
    ));
    let contender_uow = uow.clone();
    let contender_fixture = fixture.clone();
    let contender_digest = semantic_digest.clone();
    let (pid_sender, pid_receiver) = tokio::sync::oneshot::channel();
    let contender = tokio::spawn(async move {
        let mut tx = contender_uow.begin().await.expect("contender transaction");
        tx.set_local_timeouts(10_000, 10_000)
            .await
            .expect("bound contender race waits");
        contender_fixture.seed_shadows(&mut tx).await;
        let pid = count(&mut tx, "SELECT pg_catalog.pg_backend_pid()::bigint").await;
        pid_sender.send(pid).expect("report contender backend");
        let result = resolve(&mut tx, "race", &contender_digest, MessageKey::new()).await;
        let shadow_messages = count(&mut tx, "SELECT count(*) FROM pg_temp.ingress_messages").await;
        tx.commit().await.expect("contender commit");
        (result, shadow_messages)
    });
    let contender_pid = pid_receiver.await.expect("contender started");
    assert_ne!(winner_pid, contender_pid);
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT $1::integer = ANY(pg_catalog.pg_blocking_pids($2::integer))",
            )
            .bind(i32::try_from(winner_pid).expect("winner PID"))
            .bind(i32::try_from(contender_pid).expect("contender PID"))
            .fetch_one(&fixture.admin)
            .await
            .expect("observe candidate conflict");
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("contender waits on the winner's alias");
    winner.commit().await.expect("winner commit");
    let (result, shadow_messages) = contender.await.expect("contender task");
    assert!(
        matches!(result, AliasResolution::Aliased(AliasOutcome::Existing(saved)) if saved == race_key)
    );
    assert_eq!(shadow_messages, 0);
    let mut check = uow.begin().await.expect("post-race transaction");
    assert_eq!(
        count(
            &mut check,
            &format!("SELECT count(*) FROM {}.ingress_messages", fixture.target)
        )
        .await,
        2,
        "the losing candidate must be removed from the trusted schema"
    );
    assert_eq!(
        count(
            &mut check,
            &format!(
                "SELECT count(*) FROM {}.ingress_origin_aliases",
                fixture.target
            )
        )
        .await,
        2
    );
    check.commit().await.expect("post-race check");
    drop(main_uow);

    for relation in ["ingress_origin_aliases", "ingress_messages"] {
        let mut missing = uow.begin().await.expect("missing relation transaction");
        fixture.seed_shadows(&mut missing).await;
        let renamed = format!("{relation}_held");
        sqlx::query(&format!(
            "ALTER TABLE {}.{relation} RENAME TO {renamed}",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await
        .expect("rename trusted relation");
        // The unchanged raw API is a negative control: under this same
        // transaction and path it writes to the temporary shadows.
        missing
            .transaction_mut()
            .execute("UPDATE pg_temp.ingress_protocol_epoch SET epoch = 1", ())
            .await
            .expect("enable shadow epoch for negative control");
        let raw = crate::ingress_substrate::resolve_and_record_alias(
            missing.transaction_mut(),
            &"alice@example.test".parse().expect("sender"),
            &NormalizedTarget::Bare("bob@example.test".parse().expect("target")),
            &OriginId::new("shadow-only"),
            &semantic_digest,
            MessageKey::new,
        )
        .await
        .expect("raw path follows temporary shadows");
        assert!(matches!(
            raw,
            AliasResolution::Aliased(AliasOutcome::Inserted(_))
        ));
        assert_eq!(
            count(
                &mut missing,
                "SELECT count(*) FROM pg_temp.ingress_origin_aliases"
            )
            .await,
            1
        );
        let result = CanonicalMessageRepository::resolve_and_record_alias(
            &mut missing,
            &"alice@example.test".parse().expect("sender"),
            &NormalizedTarget::Bare("bob@example.test".parse().expect("target")),
            &OriginId::new("missing"),
            &semantic_digest,
            MessageKey::new,
        )
        .await;
        assert!(
            result.is_err(),
            "missing trusted {relation} must not fall through to pg_temp"
        );
        drop(missing);
        sqlx::query(&format!(
            "ALTER TABLE {}.{renamed} RENAME TO {relation}",
            fixture.target
        ))
        .execute(&fixture.admin)
        .await
        .expect("restore trusted relation");
    }
    drop(uow);
    drop(pool);
}

#[tokio::test]
async fn sqlite_shared_memory_canonical_replay_and_rollback() {
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, ":memory:");
    config.pool_size = 1;
    let pool = DatabasePool::try_new(config, PoolConfig)
        .await
        .expect("shared SQLite pool");
    MigrationRunner::single()
        .run(pool.global())
        .await
        .expect("SQLite migrations");
    let uow = IngressUnitOfWork::open(
        pool.ingress(),
        LineageConfig {
            deployment_uuid: None,
            action: None,
        },
    )
    .expect("SQLite UoW");
    let key = MessageKey::new();
    let semantic_digest = digest(4);
    let mut tx = uow.begin().await.expect("transaction");
    assert!(tx.receipt_schema().is_none());
    assert!(
        matches!(resolve(&mut tx, "sqlite", &semantic_digest, key).await, AliasResolution::Aliased(AliasOutcome::Inserted(saved)) if saved == key)
    );
    let message = envelope("sqlite");
    CanonicalMessageRepository::record_message(&mut tx, key, &semantic_digest, Some(&message))
        .await
        .expect("record");
    tx.commit().await.expect("commit");
    let mut replay = uow.begin().await.expect("replay");
    assert!(
        matches!(resolve(&mut replay, "sqlite", &semantic_digest, MessageKey::new()).await, AliasResolution::Aliased(AliasOutcome::Existing(saved)) if saved == key)
    );
    assert_eq!(
        CanonicalMessageRepository::load_envelope(&mut replay, key)
            .await
            .expect("load"),
        Some(message)
    );
    assert!(matches!(
        resolve(
            &mut replay,
            "rolled-back",
            &semantic_digest,
            MessageKey::new()
        )
        .await,
        AliasResolution::Aliased(AliasOutcome::Inserted(_))
    ));
    drop(replay);
    let mut check = uow.begin().await.expect("post rollback");
    assert_eq!(
        count(&mut check, "SELECT count(*) FROM ingress_origin_aliases").await,
        1
    );
    assert_eq!(
        count(&mut check, "SELECT count(*) FROM ingress_messages").await,
        1
    );
    check.commit().await.expect("check commit");
}
