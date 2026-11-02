//! The authenticated principal assertion must lock the attested session row.

use super::{count, Fixture};
use crate::config::LineageConfig;
use crate::db::physical_attestation::attested_pool;
use crate::db::{
    Database, DatabaseConfig, DatabaseDriver, DatabasePool, MigrationRunner, PoolConfig,
};
use crate::ingress_uow::{
    IngressUnitOfWork, IngressUowTransaction, PrincipalAssertion, PrincipalRepository,
};
use futures::FutureExt;
use sqlx::postgres::PgPoolOptions;
use std::time::Duration;
use uuid::Uuid;
use waddle_xmpp::auth::{
    AuthContextId, AuthContextVersion, AuthenticatedPrincipalRef, PrincipalAuthEpoch,
};

const EXPIRED: &str = "2000-01-01T00:00:00Z";

fn principal() -> AuthenticatedPrincipalRef {
    AuthenticatedPrincipalRef::new(
        "alice@example.test".parse().expect("valid principal"),
        AuthContextId::new(Uuid::new_v4()),
        AuthContextVersion::new(3),
        PrincipalAuthEpoch::new(5),
    )
}

async fn seed_target_user(fixture: &Fixture) {
    sqlx::query(&format!("INSERT INTO {}.users (jid, username, xmpp_localpart, created_at, updated_at) VALUES ($1, $2, $3, $4, $4)", fixture.target))
        .bind("alice@example.test")
        .bind("alice")
        .bind("alice")
        .bind("2020-01-01T00:00:00Z")
        .execute(&fixture.admin).await.expect("target user");
}

async fn seed_target_session(
    fixture: &Fixture,
    reference: &AuthenticatedPrincipalRef,
    expiry: Option<&str>,
) -> String {
    let id = Uuid::new_v4().to_string();
    sqlx::query(&format!("INSERT INTO {}.sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, expires_at, created_at, last_used_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $8)", fixture.target))
        .bind(&id)
        .bind(reference.bare_jid().to_string())
        .bind(Uuid::new_v4().to_string())
        .bind(reference.auth_context_id().as_uuid().to_string())
        .bind(i64::try_from(reference.auth_context_version().get()).expect("version"))
        .bind(i64::try_from(reference.auth_epoch().get()).expect("epoch"))
        .bind(expiry)
        .bind("2020-01-01T00:00:00Z")
        .execute(&fixture.admin).await.expect("target session");
    id
}

async fn seed_attacker_session(
    fixture: &Fixture,
    schema: &str,
    reference: &AuthenticatedPrincipalRef,
    expiry: Option<&str>,
) {
    sqlx::query(&format!("INSERT INTO {schema}.sessions (user_jid, auth_context_id, auth_context_version, principal_auth_epoch, expires_at) VALUES ($1, $2, $3, $4, $5)"))
        .bind(reference.bare_jid().to_string())
        .bind(reference.auth_context_id().as_uuid().to_string())
        .bind(i64::try_from(reference.auth_context_version().get()).expect("version"))
        .bind(i64::try_from(reference.auth_epoch().get()).expect("epoch"))
        .bind(expiry)
        .execute(&fixture.admin).await.expect("attacker session");
}

async fn use_hostile_path(tx: &mut IngressUowTransaction<'_>, attacker: &str, target: &str) {
    tx.transaction_mut()
        .execute(&format!("SET LOCAL search_path = {attacker}, {target}"), ())
        .await
        .expect("hostile path");
}

async fn raw_expiry(
    tx: &mut IngressUowTransaction<'_>,
    reference: &AuthenticatedPrincipalRef,
) -> Option<Option<String>> {
    let mut rows = tx.transaction_mut().query(
        "SELECT expires_at FROM sessions WHERE user_jid = ? AND auth_context_id = ? AND auth_context_version = ? AND principal_auth_epoch = ? FOR SHARE",
        crate::db_params![
            reference.bare_jid().to_string(),
            reference.auth_context_id().as_uuid().to_string(),
            i64::try_from(reference.auth_context_version().get()).expect("version"),
            i64::try_from(reference.auth_epoch().get()).expect("epoch"),
        ],
    ).await.expect("raw assertion control");
    rows.next()
        .await
        .expect("raw assertion row")
        .map(|row| row.get(0).expect("expiry"))
}

#[tokio::test]
async fn attested_principal_ignores_hostile_schema_and_locks_trusted_session() {
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
    let attacker = format!("principal_shadow_{}", Uuid::new_v4().simple());
    for sql in [
        format!("CREATE SCHEMA {attacker}"),
        format!("CREATE TABLE {attacker}.sessions (user_jid text NOT NULL, auth_context_id text NOT NULL, auth_context_version bigint NOT NULL, principal_auth_epoch bigint NOT NULL, expires_at text)"),
        format!("GRANT USAGE ON SCHEMA {attacker} TO {}", fixture.runtime),
        format!("GRANT SELECT, UPDATE ON {attacker}.sessions TO {}", fixture.runtime),
    ] {
        sqlx::query(&sql).execute(&fixture.admin).await.expect("attacker setup");
    }
    let valid = principal();
    let missing = principal();
    let expired = principal();
    seed_target_user(fixture).await;
    let valid_id = seed_target_session(fixture, &valid, None).await;
    seed_target_session(fixture, &expired, Some(EXPIRED)).await;
    seed_attacker_session(fixture, &attacker, &valid, Some(EXPIRED)).await;
    seed_attacker_session(fixture, &attacker, &missing, None).await;
    seed_attacker_session(fixture, &attacker, &expired, None).await;

    let attested = attested_pool(
        fixture.runtime_options.clone(),
        fixture.config.clone(),
        2,
        Duration::from_secs(2),
    )
    .expect("attested pool");
    let pool = DatabasePool::from_attested_postgres_pool("principal fixture", attested, 1, 1)
        .await
        .expect("shared pool");
    let main = pool.global().begin().await.expect("hold main slot");
    assert!(
        tokio::time::timeout(Duration::from_millis(100), pool.global().begin())
            .await
            .is_err(),
        "main slot is occupied"
    );
    let uow = IngressUnitOfWork::open(
        pool.ingress(),
        LineageConfig {
            deployment_uuid: Some(fixture.lineage.deployment_uuid),
            action: None,
        },
    )
    .expect("ingress UoW");
    let mut tx = uow.begin().await.expect("prepared transaction");
    tx.set_local_timeouts(10_000, 10_000)
        .await
        .expect("bound lock test");
    assert!(tx.receipt_schema().is_some());
    let pid = count(&mut tx, "SELECT pg_catalog.pg_backend_pid()::bigint").await;
    assert_eq!(count(&mut tx, "SELECT CASE WHEN pg_catalog.has_database_privilege(current_user, pg_catalog.current_database(), 'TEMP') THEN 1::bigint ELSE 0::bigint END").await, 0);
    use_hostile_path(&mut tx, &attacker, &fixture.target).await;
    assert_eq!(
        raw_expiry(&mut tx, &valid).await,
        Some(Some(EXPIRED.to_owned()))
    );
    assert_eq!(
        PrincipalRepository::assert_principal(&mut tx, &valid)
            .await
            .expect("trusted valid"),
        PrincipalAssertion::Asserted
    );
    assert_eq!(raw_expiry(&mut tx, &missing).await, Some(None));
    assert_eq!(
        PrincipalRepository::assert_principal(&mut tx, &missing)
            .await
            .expect("missing trusted row"),
        PrincipalAssertion::PrincipalAssertionFailed
    );
    assert_eq!(raw_expiry(&mut tx, &expired).await, Some(None));
    assert_eq!(
        PrincipalRepository::assert_principal(&mut tx, &expired)
            .await
            .expect("trusted expiry"),
        PrincipalAssertion::PrincipalAssertionFailed
    );
    assert_eq!(
        count(&mut tx, "SELECT pg_catalog.pg_backend_pid()::bigint").await,
        pid
    );

    // The trusted FOR SHARE lock, not the identically keyed attacker row,
    // must keep a real session revocation waiting until this transaction ends.
    let admin = fixture.admin.clone();
    let target = fixture.target.clone();
    let (pid_sender, pid_receiver) = tokio::sync::oneshot::channel();
    let revocation = tokio::spawn(async move {
        let mut connection = admin.acquire().await.expect("revocation connection");
        let revoke_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
            .fetch_one(&mut *connection)
            .await
            .expect("revocation PID");
        pid_sender.send(revoke_pid).expect("send revocation PID");
        tokio::time::timeout(Duration::from_secs(15), async {
            sqlx::query(&format!("DELETE FROM {target}.sessions WHERE id = $1"))
                .bind(valid_id)
                .execute(&mut *connection)
                .await
        })
        .await
        .expect("bound revocation wait")
        .expect("delete trusted session")
        .rows_affected()
    });
    let revoke_pid = tokio::time::timeout(Duration::from_secs(5), pid_receiver)
        .await
        .expect("bound revocation startup")
        .expect("revocation started");
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let blocked: bool = sqlx::query_scalar(
                "SELECT $1::integer = ANY(pg_catalog.pg_blocking_pids($2::integer))",
            )
            .bind(i32::try_from(pid).expect("assertion PID"))
            .bind(revoke_pid)
            .fetch_one(&fixture.admin)
            .await
            .expect("observe lock wait");
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("revocation waits for trusted share lock");
    tx.commit().await.expect("release assertion lock");
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(5), revocation)
            .await
            .expect("revocation finishes")
            .expect("revocation task"),
        1
    );
    sqlx::query(&format!(
        "UPDATE {attacker}.sessions SET expires_at = NULL WHERE auth_context_id = $1"
    ))
    .bind(valid.auth_context_id().as_uuid().to_string())
    .execute(&fixture.admin)
    .await
    .expect("make forged row valid");
    let mut after = uow.begin().await.expect("after revocation");
    assert_eq!(
        count(&mut after, "SELECT pg_catalog.pg_backend_pid()::bigint").await,
        pid,
        "same ingress backend"
    );
    use_hostile_path(&mut after, &attacker, &fixture.target).await;
    assert_eq!(raw_expiry(&mut after, &valid).await, Some(None));
    assert_eq!(
        PrincipalRepository::assert_principal(&mut after, &valid)
            .await
            .expect("revoked"),
        PrincipalAssertion::PrincipalAssertionFailed
    );
    after.commit().await.expect("finish revocation check");

    let mut absent_table = uow.begin().await.expect("missing table transaction");
    use_hostile_path(&mut absent_table, &attacker, &fixture.target).await;
    sqlx::query(&format!(
        "ALTER TABLE {}.sessions RENAME TO sessions_held",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("hide trusted sessions");
    assert_eq!(raw_expiry(&mut absent_table, &missing).await, Some(None));
    assert!(
        PrincipalRepository::assert_principal(&mut absent_table, &missing)
            .await
            .is_err(),
        "missing trusted table must fail closed"
    );
    drop(absent_table);
    sqlx::query(&format!(
        "ALTER TABLE {}.sessions_held RENAME TO sessions",
        fixture.target
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore trusted sessions");

    // The unprepared PostgreSQL API intentionally retains its legacy search
    // path behavior. Use a separate one-slot owner pool as the raw control.
    let raw_pool = PgPoolOptions::new()
        .max_connections(1)
        .connect_with(fixture.admin.connect_options().as_ref().clone())
        .await
        .expect("raw PostgreSQL pool");
    let raw_db = Database::from_postgres_pool("raw principal fixture", raw_pool.clone());
    {
        let connection = raw_db.guard().await.expect("raw connection");
        connection
            .execute(&format!("SET search_path = {}", fixture.target), ())
            .await
            .expect("raw initial path");
    }
    let raw_uow = IngressUnitOfWork::open(
        raw_db,
        LineageConfig {
            deployment_uuid: Some(fixture.lineage.deployment_uuid),
            action: None,
        },
    )
    .expect("raw PostgreSQL UoW");
    let mut raw_tx = raw_uow.begin().await.expect("raw PostgreSQL transaction");
    assert!(raw_tx.receipt_schema().is_none());
    use_hostile_path(&mut raw_tx, &attacker, &fixture.target).await;
    assert_eq!(
        PrincipalRepository::assert_principal(&mut raw_tx, &missing)
            .await
            .expect("raw PostgreSQL assertion"),
        PrincipalAssertion::Asserted
    );
    raw_tx.commit().await.expect("raw PostgreSQL commit");
    drop(raw_uow);
    raw_pool.close().await;
    drop(main);
    drop(uow);
    drop(pool);
}

#[tokio::test]
async fn sqlite_principal_assertion_remains_on_shared_pool() {
    let mut config = DatabaseConfig::new(DatabaseDriver::Sqlite, ":memory:");
    config.pool_size = 1;
    let pool = DatabasePool::try_new(config, PoolConfig)
        .await
        .expect("shared SQLite pool");
    MigrationRunner::single()
        .run(pool.global())
        .await
        .expect("migrations");
    let uow = IngressUnitOfWork::open(
        pool.ingress(),
        LineageConfig {
            deployment_uuid: None,
            action: None,
        },
    )
    .expect("SQLite UoW");
    let valid = principal();
    let missing = principal();
    let expired = principal();
    let mut tx = uow.begin().await.expect("seed transaction");
    assert!(tx.receipt_schema().is_none());
    tx.transaction_mut().execute(
        "INSERT INTO users (jid, username, xmpp_localpart, created_at, updated_at) VALUES (?, ?, ?, ?, ?)",
        crate::db_params!["alice@example.test", "alice", "alice", "2020-01-01T00:00:00Z", "2020-01-01T00:00:00Z"],
    ).await.expect("user");
    for (reference, expiry) in [(&valid, None), (&expired, Some(EXPIRED))] {
        tx.transaction_mut().execute(
            "INSERT INTO sessions (id, user_jid, token_hash, auth_context_id, auth_context_version, principal_auth_epoch, expires_at, created_at, last_used_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
            crate::db_params![
                Uuid::new_v4().to_string(), reference.bare_jid().to_string(), Uuid::new_v4().to_string(),
                reference.auth_context_id().as_uuid().to_string(),
                i64::try_from(reference.auth_context_version().get()).expect("version"),
                i64::try_from(reference.auth_epoch().get()).expect("epoch"),
                expiry, "2020-01-01T00:00:00Z", "2020-01-01T00:00:00Z",
            ],
        ).await.expect("session");
    }
    tx.commit().await.expect("commit seed");
    let mut tx = uow.begin().await.expect("assertion transaction");
    assert_eq!(
        PrincipalRepository::assert_principal(&mut tx, &valid)
            .await
            .expect("valid"),
        PrincipalAssertion::Asserted
    );
    assert_eq!(
        PrincipalRepository::assert_principal(&mut tx, &missing)
            .await
            .expect("missing"),
        PrincipalAssertion::PrincipalAssertionFailed
    );
    assert_eq!(
        PrincipalRepository::assert_principal(&mut tx, &expired)
            .await
            .expect("expired"),
        PrincipalAssertion::PrincipalAssertionFailed
    );
    let wrong_version = AuthenticatedPrincipalRef::new(
        valid.bare_jid().clone(),
        valid.auth_context_id().clone(),
        AuthContextVersion::new(4),
        valid.auth_epoch(),
    );
    assert_eq!(
        PrincipalRepository::assert_principal(&mut tx, &wrong_version)
            .await
            .expect("wrong version"),
        PrincipalAssertion::PrincipalAssertionFailed
    );
    tx.commit().await.expect("commit assertion");
}
