use std::str::FromStr;
use std::time::Duration;

use sqlx::postgres::types::Oid;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Row};
use tokio::time::{sleep, timeout};
use uuid::Uuid;

use super::{
    acquire_dark_shared_migration_ledger_v1023, acquire_dark_shared_migration_ledger_v1030_read,
    acquire_dark_shared_migration_ledger_v1030_write, DarkLedgerPreflightError,
    DarkMigrationLedgerPins, MIGRATION_LEDGER_ADVISORY_LOCK_KEY,
};
use crate::db::lineage::LINEAGE_ADVISORY_LOCK_KEY;
use crate::db::migrations::{migration_checksum, MigrationRunner};
use crate::db::{Database, DatabaseDriver, DatabaseError, MigrationLedgerError};

const RESTORE_PATH: &str = "\"$user\", public";
const CUSTOM_RESTORE_PATH: &str = "public, pg_catalog";

struct Fixture {
    admin: PgPool,
    bootstrap_admin: Option<PgPool>,
    reader: PgPool,
    database_name: Option<String>,
    schema: String,
    owner: String,
    runtime: String,
}

#[tokio::test]
async fn postgres17_ledger_preflight_v1030_read_only_requires_complete_prefix_and_boundary() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new_without_temp(&url).await;
    fixture.seed_v1030().await;
    let db = fixture.database();
    let pins = fixture.pins().await;

    let mut tx = db.begin().await.expect("read-only transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await
    .expect("read-only boundary");
    let mut proof = acquire_dark_shared_migration_ledger_v1030_read(
        &mut tx,
        &fixture.schema,
        pins,
        RESTORE_PATH,
    )
    .await
    .expect("complete registered V1030 prefix");
    assert!(holds_shared_advisory_lock(proof.transaction(), LINEAGE_ADVISORY_LOCK_KEY).await);
    assert!(
        holds_shared_advisory_lock(proof.transaction(), MIGRATION_LEDGER_ADVISORY_LOCK_KEY).await
    );
    let boundary: (String, String) = sqlx::query_as(
        "SELECT pg_catalog.current_setting('transaction_isolation'), \
                pg_catalog.current_setting('transaction_read_only')",
    )
    .fetch_one(
        &mut *proof
            .transaction()
            .postgres_connection()
            .expect("postgres tx"),
    )
    .await
    .expect("read transaction boundary");
    assert_eq!(boundary, ("read committed".to_owned(), "on".to_owned()));
    drop(proof);
    tx.rollback().await.expect("release read-only proof");

    let mut tx = db.begin().await.expect("read-write transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE",
        (),
    )
    .await
    .expect("read-write boundary");
    assert!(matches!(
        acquire_dark_shared_migration_ledger_v1030_read(
            &mut tx,
            &fixture.schema,
            pins,
            RESTORE_PATH
        )
        .await,
        Err(DarkLedgerPreflightError::TransactionBoundary)
    ));
    assert!(!holds_shared_advisory_lock(&mut tx, LINEAGE_ADVISORY_LOCK_KEY).await);
    assert!(!holds_shared_advisory_lock(&mut tx, MIGRATION_LEDGER_ADVISORY_LOCK_KEY).await);
    tx.rollback().await.expect("rollback rejected boundary");

    sqlx::query(&format!(
        "DELETE FROM {}._migrations WHERE version=1030",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove current migration from fixture");
    let mut tx = db.begin().await.expect("missing migration transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await
    .expect("read-only boundary");
    assert!(matches!(
        acquire_dark_shared_migration_ledger_v1030_read(
            &mut tx,
            &fixture.schema,
            pins,
            RESTORE_PATH
        )
        .await,
        Err(DarkLedgerPreflightError::Database(
            DatabaseError::MigrationLedger(MigrationLedgerError::PendingMigration {
                version: 1030
            })
        ))
    ));
    tx.rollback().await.expect("rollback missing migration");
    fixture.close().await;
}

#[tokio::test]
async fn postgres17_ledger_preflight_v1030_read_only_locks_in_order() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new_without_temp(&url).await;
    fixture.seed_v1030().await;
    let db = fixture.database();
    let pins = fixture.pins().await;

    let mut lineage_blocker = Some(fixture.admin.begin().await.expect("lineage blocker"));
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(LINEAGE_ADVISORY_LOCK_KEY)
        .execute(&mut **lineage_blocker.as_mut().expect("lineage blocker"))
        .await
        .expect("hold exclusive lineage lock");
    let mut ledger_blocker = Some(fixture.admin.begin().await.expect("ledger blocker"));
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(MIGRATION_LEDGER_ADVISORY_LOCK_KEY)
        .execute(&mut **ledger_blocker.as_mut().expect("ledger blocker"))
        .await
        .expect("hold exclusive ledger lock");
    let mut relation_blocker = Some(fixture.admin.begin().await.expect("relation blocker"));
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}._migrations IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut **relation_blocker.as_mut().expect("relation blocker"))
    .await
    .expect("hold exclusive ledger relation lock");

    let mut tx = db.begin().await.expect("read-only transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await
    .expect("read-only boundary");
    let backend_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *tx.postgres_connection().expect("postgres tx"))
        .await
        .expect("backend pid");
    let mut proof_future = Box::pin(acquire_dark_shared_migration_ledger_v1030_read(
        &mut tx,
        &fixture.schema,
        pins,
        RESTORE_PATH,
    ));

    // Each tuple records (lineage held, lineage waiting, ledger held, ledger
    // waiting, relation held, relation waiting) on the proof's backend.
    for (stage, expected) in [
        ("lineage", (false, true, false, false, false, false)),
        ("ledger", (true, false, false, true, false, false)),
        ("relation", (true, false, true, false, false, true)),
    ] {
        tokio::select! {
            result = &mut proof_future => match result {
                Ok(_) => panic!("proof completed before {stage} wait"),
                Err(error) => panic!("proof failed before {stage} wait: {error}"),
            },
            () = wait_for_preflight_lock_stage(&fixture.admin, backend_pid, pins.ledger_relation_oid, expected) => {}
        }
        match stage {
            "lineage" => lineage_blocker
                .take()
                .expect("lineage blocker")
                .rollback()
                .await
                .expect("release lineage blocker"),
            "ledger" => ledger_blocker
                .take()
                .expect("ledger blocker")
                .rollback()
                .await
                .expect("release ledger blocker"),
            "relation" => relation_blocker
                .take()
                .expect("relation blocker")
                .rollback()
                .await
                .expect("release relation blocker"),
            _ => unreachable!("fixed lock stages"),
        }
    }
    let mut proof = proof_future
        .await
        .expect("read-only proof after blockers release");
    assert!(holds_shared_advisory_lock(proof.transaction(), LINEAGE_ADVISORY_LOCK_KEY).await);
    assert!(
        holds_shared_advisory_lock(proof.transaction(), MIGRATION_LEDGER_ADVISORY_LOCK_KEY).await
    );
    drop(proof);
    tx.rollback().await.expect("release proof locks");
    fixture.close().await;
}

impl Fixture {
    async fn new(url: &str) -> Self {
        Self::new_in_database(url, None, None).await
    }

    async fn new_without_temp(url: &str) -> Self {
        let bootstrap_admin = PgPool::connect(url)
            .await
            .expect("bootstrap admin connection");
        let database_name = format!("ledger_no_temp_{}", Uuid::new_v4().simple());
        sqlx::query(&format!(
            "CREATE DATABASE {database_name} TEMPLATE template0"
        ))
        .execute(&bootstrap_admin)
        .await
        .expect("create isolated no-temp database");
        sqlx::query(&format!(
            "REVOKE TEMP ON DATABASE {database_name} FROM PUBLIC"
        ))
        .execute(&bootstrap_admin)
        .await
        .expect("remove public TEMP privilege");
        sqlx::query(&format!(
            "GRANT CONNECT ON DATABASE {database_name} TO PUBLIC"
        ))
        .execute(&bootstrap_admin)
        .await
        .expect("allow isolated fixture connections");
        Self::new_in_database(url, Some(database_name), Some(bootstrap_admin)).await
    }

    async fn new_in_database(
        url: &str,
        database_name: Option<String>,
        bootstrap_admin: Option<PgPool>,
    ) -> Self {
        let mut admin_options = PgConnectOptions::from_str(url).expect("connection options");
        if let Some(database_name) = &database_name {
            admin_options = admin_options.database(database_name);
        }
        let admin = PgPoolOptions::new()
            .connect_with(admin_options)
            .await
            .expect("admin connection");
        let version: i32 =
            sqlx::query_scalar("SELECT pg_catalog.current_setting('server_version_num')::integer")
                .fetch_one(&admin)
                .await
                .expect("server version");
        assert!(
            version >= 170_000,
            "fixture requires PostgreSQL 17 or later"
        );
        let suffix = Uuid::new_v4().simple().to_string();
        let schema = format!("ledger_p21_{suffix}");
        let owner = format!("ledger_o_{suffix}");
        let runtime = format!("ledger_r_{suffix}");
        for statement in [
            format!("CREATE ROLE {owner} NOLOGIN"),
            format!("CREATE ROLE {runtime} LOGIN"),
            format!("CREATE SCHEMA {schema} AUTHORIZATION {owner}"),
            format!("GRANT USAGE ON SCHEMA {schema} TO {runtime}"),
        ] {
            sqlx::query(&statement)
                .execute(&admin)
                .await
                .expect("setup");
        }
        let mut options = PgConnectOptions::from_str(url)
            .expect("connection options")
            .username(&runtime);
        if let Some(database_name) = &database_name {
            options = options.database(database_name);
        }
        let reader = PgPoolOptions::new()
            .max_connections(1)
            .connect_with(options)
            .await
            .expect("runtime connection");
        let fixture = Self {
            admin,
            bootstrap_admin,
            reader,
            database_name,
            schema,
            owner,
            runtime,
        };
        fixture.create_ledger().await;
        fixture
    }

    fn database(&self) -> Database {
        Database::from_postgres_pool("ledger-preflight", self.reader.clone())
    }

    async fn create_ledger(&self) {
        let mut conn = self.admin.acquire().await.expect("owner connection");
        sqlx::query(&format!("SET ROLE {}", self.owner))
            .execute(&mut *conn)
            .await
            .expect("assume owner");
        sqlx::query(&format!(
            "CREATE TABLE {}._migrations (version BIGINT PRIMARY KEY, description TEXT NOT NULL, checksum TEXT, applied_at TIMESTAMPTZ NOT NULL DEFAULT CURRENT_TIMESTAMP)",
            self.schema
        ))
        .execute(&mut *conn)
        .await
        .expect("create ledger");
        sqlx::query(&format!(
            "GRANT SELECT ON {}._migrations TO {}",
            self.schema, self.runtime
        ))
        .execute(&mut *conn)
        .await
        .expect("grant reader");
        sqlx::query(&format!(
            "CREATE TABLE IF NOT EXISTS {}.write_probe (id integer PRIMARY KEY)",
            self.schema
        ))
        .execute(&mut *conn)
        .await
        .expect("create durable write probe");
        sqlx::query(&format!(
            "GRANT INSERT ON {}.write_probe TO {}",
            self.schema, self.runtime
        ))
        .execute(&mut *conn)
        .await
        .expect("grant durable write");
        for migration in MigrationRunner::single()
            .migrations
            .into_iter()
            .filter(|migration| migration.version <= 1023)
        {
            sqlx::query(&format!(
                "INSERT INTO {}._migrations (version, description, checksum) VALUES ($1, $2, $3)",
                self.schema
            ))
            .bind(migration.version)
            .bind(&migration.description)
            .bind(migration_checksum(&migration, DatabaseDriver::Postgres))
            .execute(&mut *conn)
            .await
            .expect("seed committed prefix");
        }
        sqlx::query("RESET ROLE")
            .execute(&mut *conn)
            .await
            .expect("reset role");
    }

    async fn pins(&self) -> DarkMigrationLedgerPins {
        let row = sqlx::query(
            "SELECT n.oid, n.nspowner, r.oid, c.oid, COALESCE(i.indexrelid, c.oid) FROM pg_catalog.pg_namespace n \
             JOIN pg_catalog.pg_class c ON c.relnamespace=n.oid AND c.relname='_migrations' \
             LEFT JOIN pg_catalog.pg_index i ON i.indrelid=c.oid AND i.indisprimary \
             JOIN pg_catalog.pg_roles r ON r.rolname=$2 WHERE n.nspname=$1",
        )
        .bind(&self.schema)
        .bind(&self.runtime)
        .fetch_one(&self.admin)
        .await
        .expect("fixture pins");
        DarkMigrationLedgerPins {
            schema_oid: row.get::<Oid, _>(0).0,
            schema_owner_role_oid: row.get::<Oid, _>(1).0,
            runtime_role_oid: row.get::<Oid, _>(2).0,
            ledger_relation_oid: row.get::<Oid, _>(3).0,
            ledger_primary_index_oid: row.get::<Oid, _>(4).0,
        }
    }

    async fn reset_ledger(&self) {
        sqlx::query(&format!("DROP TABLE {}._migrations CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .expect("drop test ledger");
        self.create_ledger().await;
    }

    async fn seed_v1030(&self) {
        let mut conn = self.admin.acquire().await.expect("owner connection");
        sqlx::query(&format!("SET ROLE {}", self.owner))
            .execute(&mut *conn)
            .await
            .expect("assume owner");
        for migration in MigrationRunner::single()
            .migrations
            .into_iter()
            .filter(|migration| (1023..=1030).contains(&migration.version))
        {
            if migration.version == 1023 {
                continue;
            }
            sqlx::query(&format!(
                "INSERT INTO {}._migrations (version, description, checksum) VALUES ($1, $2, $3)",
                self.schema
            ))
            .bind(migration.version)
            .bind(&migration.description)
            .bind(migration_checksum(&migration, DatabaseDriver::Postgres))
            .execute(&mut *conn)
            .await
            .expect("seed current registered migration");
        }
        sqlx::query("RESET ROLE")
            .execute(&mut *conn)
            .await
            .expect("reset role");
    }

    async fn close(self) {
        self.reader.close().await;
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&self.admin)
            .await
            .expect("drop schema");
        for role in [&self.runtime, &self.owner] {
            sqlx::query(&format!("DROP ROLE {role}"))
                .execute(&self.admin)
                .await
                .expect("drop role");
        }
        self.admin.close().await;
        if let Some(database_name) = self.database_name {
            let bootstrap_admin = self
                .bootstrap_admin
                .expect("bootstrap admin for isolated DB");
            sqlx::query(&format!("DROP DATABASE {database_name}"))
                .execute(&bootstrap_admin)
                .await
                .expect("drop isolated no-temp database");
            bootstrap_admin.close().await;
        }
    }
}

#[tokio::test]
async fn postgres17_ledger_preflight_v1030_read_write_and_catalog_fail_closed() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new_without_temp(&url).await;
    fixture.seed_v1030().await;
    let runtime_has_temp: bool = sqlx::query_scalar(
        "SELECT pg_catalog.has_database_privilege(current_user, pg_catalog.current_database(), 'TEMP')"
    ).fetch_one(&fixture.reader).await.expect("inspect effective TEMP privilege");
    assert!(
        !runtime_has_temp,
        "durable write fixture must have no effective TEMP privilege"
    );
    let db = fixture.database();
    let pins = fixture.pins().await;

    let mut tx = db.begin().await.expect("write transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE",
        (),
    )
    .await
    .expect("read-write boundary");
    let mut proof = acquire_dark_shared_migration_ledger_v1030_write(
        &mut tx,
        &fixture.schema,
        pins,
        RESTORE_PATH,
    )
    .await
    .expect("complete registered V1030 prefix");
    assert!(holds_shared_advisory_lock(proof.transaction(), LINEAGE_ADVISORY_LOCK_KEY).await);
    assert!(
        holds_shared_advisory_lock(proof.transaction(), MIGRATION_LEDGER_ADVISORY_LOCK_KEY).await
    );
    proof
        .transaction()
        .execute(
            &format!("INSERT INTO {}.write_probe (id) VALUES (1)", fixture.schema),
            (),
        )
        .await
        .expect("write durable row after proof");
    drop(proof);
    tx.commit().await.expect("commit durable write after proof");
    let durable: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.write_probe WHERE id=1",
        fixture.schema
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("durable row");
    assert_eq!(durable, 1);

    // The write proof must reject read-only transactions without attempting
    // to upgrade or read the ledger.
    let mut tx = db.begin().await.expect("read-only transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await
    .expect("read-only boundary");
    assert!(matches!(
        acquire_dark_shared_migration_ledger_v1030_write(
            &mut tx,
            &fixture.schema,
            pins,
            RESTORE_PATH
        )
        .await,
        Err(DarkLedgerPreflightError::TransactionBoundary)
    ));
    tx.rollback().await.expect("rollback rejected transaction");

    // A blocker proves the write proof waits on lineage before acquiring the
    // ledger gate. Its stricter timeout remains in force.
    let mut blocker = fixture.admin.begin().await.expect("lineage blocker");
    sqlx::query("SELECT pg_catalog.pg_advisory_xact_lock($1)")
        .bind(LINEAGE_ADVISORY_LOCK_KEY)
        .execute(&mut *blocker)
        .await
        .expect("hold exclusive lineage lock");
    let blocker_holds_lineage: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks \
         WHERE pid=pg_catalog.pg_backend_pid() AND locktype='advisory' \
           AND mode='ExclusiveLock' AND granted AND objsubid=1 \
           AND classid=(($1::bigint >> 32)::oid) \
           AND objid=(($1::bigint & 4294967295)::oid))",
    )
    .bind(LINEAGE_ADVISORY_LOCK_KEY)
    .fetch_one(&mut *blocker)
    .await
    .expect("verify exclusive lineage lock");
    assert!(blocker_holds_lineage);
    let mut tx = db.begin().await.expect("blocked transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE",
        (),
    )
    .await
    .expect("write boundary");
    tx.execute("SET LOCAL lock_timeout = '200ms'", ())
        .await
        .expect("short lineage timeout");
    let backend_pid: i32 = sqlx::query_scalar("SELECT pg_catalog.pg_backend_pid()")
        .fetch_one(&mut *tx.postgres_connection().expect("postgres tx"))
        .await
        .expect("backend pid");
    let mut proof_future = Box::pin(acquire_dark_shared_migration_ledger_v1030_write(
        &mut tx,
        &fixture.schema,
        pins,
        RESTORE_PATH,
    ));
    let mut lineage_wait_observed = false;
    let mut ledger_lock_observed_while_waiting = false;
    let result = loop {
        match timeout(Duration::from_millis(10), &mut proof_future).await {
            Ok(result) => break result,
            Err(_) => {
                let (lineage_waiting, ledger_lock): (bool, bool) = sqlx::query_as(
                    "SELECT \
                     EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 AND locktype='advisory' \
                       AND objsubid=1 AND classid=(($2::bigint >> 32)::oid) \
                       AND objid=(($2::bigint & 4294967295)::oid) AND mode='ShareLock' AND NOT granted), \
                     EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 AND locktype='advisory' \
                       AND objsubid=1 AND classid=(($3::bigint >> 32)::oid) \
                       AND objid=(($3::bigint & 4294967295)::oid))",
                )
                .bind(backend_pid)
                .bind(LINEAGE_ADVISORY_LOCK_KEY)
                .bind(MIGRATION_LEDGER_ADVISORY_LOCK_KEY)
                .fetch_one(&fixture.admin)
                .await
                .expect("inspect blocked lock order");
                if lineage_waiting {
                    lineage_wait_observed = true;
                    ledger_lock_observed_while_waiting |= ledger_lock;
                }
            }
        }
    };
    drop(proof_future);
    let sqlstate = match result {
        Err(error) => error_sqlstate(&error),
        Ok(proof) => {
            drop(proof);
            None
        }
    };
    assert_eq!(
        sqlstate.as_deref(),
        Some("55P03"),
        "must fail on lock timeout"
    );
    assert!(
        lineage_wait_observed,
        "must observe the ungranted lineage advisory lock"
    );
    assert!(
        !ledger_lock_observed_while_waiting,
        "ledger lock must not be acquired while lineage waits"
    );
    let ledger_lock: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 \
         AND locktype='advisory' AND objsubid=1 \
         AND classid=(($2::bigint >> 32)::oid) AND objid=(($2::bigint & 4294967295)::oid))",
    )
    .bind(backend_pid)
    .bind(MIGRATION_LEDGER_ADVISORY_LOCK_KEY)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect ledger lock");
    assert!(
        !ledger_lock,
        "ledger lock must not precede the blocked lineage lock"
    );
    tx.rollback().await.expect("rollback blocked transaction");
    blocker.rollback().await.expect("release lineage blocker");

    sqlx::query(&format!(
        "UPDATE {}._migrations SET checksum='bad' WHERE version=1030",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("damage current migration");
    let mut tx = db.begin().await.expect("stale transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE",
        (),
    )
    .await
    .expect("write boundary");
    assert!(matches!(
        acquire_dark_shared_migration_ledger_v1030_write(
            &mut tx,
            &fixture.schema,
            pins,
            RESTORE_PATH
        )
        .await,
        Err(DarkLedgerPreflightError::Database(_))
    ));
    tx.rollback().await.expect("rollback stale ledger");

    fixture.close().await;
}

#[tokio::test]
async fn postgres17_ledger_preflight_rejects_too_many_acl_items_before_expansion() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url).await;
    fixture.seed_v1030().await;
    let mut roles = Vec::new();
    for index in 0..65 {
        let role = format!("ledger_acl_{index}_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE ROLE {role} NOLOGIN"))
            .execute(&fixture.admin)
            .await
            .expect("create ACL grantee");
        sqlx::query(&format!(
            "GRANT SELECT ON {}._migrations TO {role}",
            fixture.schema
        ))
        .execute(&fixture.admin)
        .await
        .expect("grow raw ACL");
        roles.push(role);
    }
    let db = fixture.database();
    let mut tx = db.begin().await.expect("write transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE",
        (),
    )
    .await
    .expect("write boundary");
    assert!(matches!(
        acquire_dark_shared_migration_ledger_v1030_write(
            &mut tx,
            &fixture.schema,
            fixture.pins().await,
            RESTORE_PATH,
        )
        .await,
        Err(DarkLedgerPreflightError::TooManyAclEntries)
    ));
    tx.rollback().await.expect("rollback oversized ACL");
    sqlx::query(&format!(
        "REVOKE ALL ON {}._migrations FROM {}",
        fixture.schema,
        roles.join(", ")
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke test ACL roles");
    for role in roles {
        sqlx::query(&format!("DROP ROLE {role}"))
            .execute(&fixture.admin)
            .await
            .expect("drop test ACL role");
    }
    fixture.close().await;
}

#[tokio::test]
async fn postgres17_ledger_preflight_rejects_compressed_or_oversized_raw_acl_first() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url).await;
    fixture.seed_v1030().await;
    let mut roles = Vec::new();
    for index in 0..2_500 {
        let role = format!("ledger_big_acl_{index}_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE ROLE {role} NOLOGIN"))
            .execute(&fixture.admin)
            .await
            .expect("create ACL grantee");
        sqlx::query(&format!(
            "GRANT USAGE ON SCHEMA {} TO {role}",
            fixture.schema
        ))
        .execute(&fixture.admin)
        .await
        .expect("grow raw ACL");
        roles.push(role);
    }
    let (raw_size, compression): (i32, Option<String>) = sqlx::query_as(
        "SELECT pg_catalog.pg_column_size(nspacl), pg_catalog.pg_column_compression(nspacl) \
         FROM pg_catalog.pg_namespace WHERE nspname=$1",
    )
    .bind(&fixture.schema)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect raw ACL witness");
    assert!(raw_size > 0);
    assert!(
        compression.as_ref().is_some(),
        "fixture must produce a PostgreSQL-compressed ACL: {raw_size} bytes"
    );
    let db = fixture.database();
    let mut tx = db.begin().await.expect("write transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ WRITE",
        (),
    )
    .await
    .expect("write boundary");
    let result = acquire_dark_shared_migration_ledger_v1030_write(
        &mut tx,
        &fixture.schema,
        fixture.pins().await,
        RESTORE_PATH,
    )
    .await;
    match result {
        Err(DarkLedgerPreflightError::CompressedAcl) => {}
        result => {
            let outcome = result
                .as_ref()
                .map(|_| "accepted".to_owned())
                .map_err(ToString::to_string);
            panic!("compressed ACL did not fail at the compression guard stage: compression={compression:?}, raw_size={raw_size}, outcome={outcome:?}");
        }
    }
    tx.rollback().await.expect("rollback oversized ACL");
    sqlx::query(&format!(
        "REVOKE ALL ON SCHEMA {} FROM {}",
        fixture.schema,
        roles.join(", ")
    ))
    .execute(&fixture.admin)
    .await
    .expect("revoke test ACL roles");
    for role in roles {
        sqlx::query(&format!("DROP ROLE {role}"))
            .execute(&fixture.admin)
            .await
            .expect("drop test ACL role");
    }
    fixture.close().await;
}

#[tokio::test]
async fn postgres17_ledger_preflight_bounds_restore_input_and_ignores_large_inherited_path() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url).await;
    let db = fixture.database();
    let pins = fixture.pins().await;

    let mut tx = db.begin().await.expect("invalid input transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await
    .expect("read-only boundary");
    tx.execute("SET LOCAL lock_timeout = '200ms'", ())
        .await
        .expect("set tighter lock wait");
    tx.execute("SET LOCAL statement_timeout = '1s'", ())
        .await
        .expect("set tighter statement wait");
    let long_schema = "s".repeat(64);
    assert!(matches!(
        acquire_dark_shared_migration_ledger_v1023(&mut tx, &long_schema, pins, RESTORE_PATH).await,
        Err(DarkLedgerPreflightError::InvalidSchema)
    ));
    assert!(matches!(
        acquire_dark_shared_migration_ledger_v1023(
            &mut tx,
            &fixture.schema,
            pins,
            &"p".repeat(1025)
        )
        .await,
        Err(DarkLedgerPreflightError::InvalidRestorePath)
    ));
    assert!(matches!(
        acquire_dark_shared_migration_ledger_v1023(&mut tx, &fixture.schema, pins, "bad\0path")
            .await,
        Err(DarkLedgerPreflightError::InvalidRestorePath)
    ));
    let lock_count: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_catalog.pg_locks WHERE pid=pg_catalog.pg_backend_pid() \
         AND locktype='advisory' AND objsubid=1 AND granted",
    )
    .fetch_one(&mut *tx.postgres_connection().expect("postgres transaction"))
    .await
    .expect("inspect rejected inputs");
    assert_eq!(
        lock_count, 0,
        "invalid values must reject before advisory locks"
    );
    tx.rollback().await.expect("rollback invalid inputs");

    let mut tx = db.begin().await.expect("large inherited path transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await
    .expect("read-only boundary");
    tx.execute("SET LOCAL lock_timeout = '200ms'", ())
        .await
        .expect("set tighter lock wait");
    tx.execute("SET LOCAL statement_timeout = '1s'", ())
        .await
        .expect("set tighter statement wait");
    let large_path = "x".repeat(60_000);
    sqlx::query("SELECT pg_catalog.set_config('search_path', $1, true)")
        .bind(&large_path)
        .execute(&mut *tx.postgres_connection().expect("postgres transaction"))
        .await
        .expect("set large inherited path");
    let proof = acquire_dark_shared_migration_ledger_v1023(
        &mut tx,
        &fixture.schema,
        pins,
        CUSTOM_RESTORE_PATH,
    )
    .await
    .expect("bounded caller restore despite huge inherited setting");
    drop(proof);
    let restored: String = sqlx::query_scalar("SELECT pg_catalog.current_setting('search_path')")
        .fetch_one(&mut *tx.postgres_connection().expect("postgres transaction"))
        .await
        .expect("read caller restore path");
    assert_eq!(restored, CUSTOM_RESTORE_PATH);
    let waits: (String, String) = sqlx::query_as(
        "SELECT pg_catalog.current_setting('lock_timeout'), pg_catalog.current_setting('statement_timeout')"
    ).fetch_one(&mut *tx.postgres_connection().expect("postgres transaction"))
        .await.expect("read bounded waits");
    assert_eq!(waits, ("200ms".to_owned(), "1s".to_owned()));
    tx.rollback()
        .await
        .expect("rollback large-path transaction");
    fixture.close().await;
}

async fn holds_shared_advisory_lock(tx: &mut crate::db::Transaction<'_>, key: i64) -> bool {
    sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks \
         WHERE locktype='advisory' AND pid=pg_catalog.pg_backend_pid() \
           AND granted AND mode='ShareLock' AND objsubid=1 \
           AND classid=(($1::bigint >> 32)::oid) \
           AND objid=(($1::bigint & 4294967295)::oid))",
    )
    .bind(key)
    .fetch_one(&mut *tx.postgres_connection().expect("postgres transaction"))
    .await
    .expect("inspect advisory lock")
}

async fn wait_for_preflight_lock_stage(
    admin: &PgPool,
    backend_pid: i32,
    relation_oid: u32,
    expected: (bool, bool, bool, bool, bool, bool),
) {
    timeout(Duration::from_secs(3), async {
        loop {
            let locks: (bool, bool, bool, bool, bool, bool) = sqlx::query_as(
                "SELECT \
                 EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 AND locktype='advisory' \
                   AND objsubid=1 AND classid=(($2::bigint >> 32)::oid) \
                   AND objid=(($2::bigint & 4294967295)::oid) AND mode='ShareLock' AND granted), \
                 EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 AND locktype='advisory' \
                   AND objsubid=1 AND classid=(($2::bigint >> 32)::oid) \
                   AND objid=(($2::bigint & 4294967295)::oid) AND mode='ShareLock' AND NOT granted), \
                 EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 AND locktype='advisory' \
                   AND objsubid=1 AND classid=(($3::bigint >> 32)::oid) \
                   AND objid=(($3::bigint & 4294967295)::oid) AND mode='ShareLock' AND granted), \
                 EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 AND locktype='advisory' \
                   AND objsubid=1 AND classid=(($3::bigint >> 32)::oid) \
                   AND objid=(($3::bigint & 4294967295)::oid) AND mode='ShareLock' AND NOT granted), \
                 EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 AND locktype='relation' \
                   AND relation=$4 AND granted), \
                 EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 AND locktype='relation' \
                   AND relation=$4 AND mode='AccessShareLock' AND NOT granted)",
            )
            .bind(backend_pid)
            .bind(LINEAGE_ADVISORY_LOCK_KEY)
            .bind(MIGRATION_LEDGER_ADVISORY_LOCK_KEY)
            .bind(Oid(relation_oid))
            .fetch_one(admin)
            .await
            .expect("inspect read-only proof locks");
            if locks == expected {
                return;
            }
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("observe expected read-only proof lock stage");
}

fn error_sqlstate(error: &DarkLedgerPreflightError) -> Option<String> {
    match error {
        DarkLedgerPreflightError::Postgres(sqlx::Error::Database(error)) => {
            error.code().map(|code| code.into_owned())
        }
        DarkLedgerPreflightError::Database(DatabaseError::Internal(sqlx::Error::Database(
            error,
        ))) => error.code().map(|code| code.into_owned()),
        _ => None,
    }
}

async fn reject_without_relation_lock(fixture: &Fixture, pins: DarkMigrationLedgerPins) {
    let db = fixture.database();
    let mut tx = db.begin().await.expect("runtime transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await
    .expect("read-only boundary");
    tx.execute("SET LOCAL search_path = public, pg_catalog", ())
        .await
        .expect("set path");
    let result = acquire_dark_shared_migration_ledger_v1023(
        &mut tx,
        &fixture.schema,
        pins,
        CUSTOM_RESTORE_PATH,
    )
    .await;
    assert!(matches!(
        result,
        Err(DarkLedgerPreflightError::CatalogDrift)
    ));
    let mut rows = tx
        .query(
            "SELECT pg_catalog.current_setting('search_path'), count(*) \
             FROM pg_catalog.pg_locks WHERE pid = pg_catalog.pg_backend_pid() \
               AND locktype = 'relation' AND relation = ?::pg_catalog.oid",
            crate::db_params![i64::from(pins.ledger_relation_oid)],
        )
        .await
        .expect("inspect failed proof");
    let row = rows.next().await.expect("lock row").expect("aggregate row");
    assert_eq!(row.get::<String>(0).expect("path"), CUSTOM_RESTORE_PATH);
    assert_eq!(row.get::<i64>(1).expect("lock count"), 0);
    drop(rows);
    tx.rollback().await.expect("rollback failed proof");
}

async fn reject_oversized_ledger(fixture: &Fixture) {
    let db = fixture.database();
    let mut tx = db.begin().await.expect("runtime transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await
    .expect("read-only boundary");
    let result = acquire_dark_shared_migration_ledger_v1023(
        &mut tx,
        &fixture.schema,
        fixture.pins().await,
        RESTORE_PATH,
    )
    .await;
    assert!(matches!(
        result,
        Err(DarkLedgerPreflightError::Database(
            DatabaseError::MigrationLedger(MigrationLedgerError::OversizedLedger)
        ))
    ));
    tx.rollback().await.expect("rollback oversized proof");
}

#[tokio::test]
async fn postgres17_ledger_preflight_bounds_rows_and_text_before_decode() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url).await;
    for column in ["description", "checksum"] {
        sqlx::query(&format!(
            "UPDATE {}._migrations SET {column} = $1 WHERE version = 1023",
            fixture.schema
        ))
        .bind("x".repeat(1_000_000))
        .execute(&fixture.admin)
        .await
        .expect("oversize one ledger field");
        reject_oversized_ledger(&fixture).await;
        fixture.reset_ledger().await;
    }
    let extra = i32::try_from(MigrationRunner::single().migrations.len() + 2)
        .expect("bounded fixture catalog");
    sqlx::query(&format!(
        "INSERT INTO {}._migrations (version, description, checksum) \
         SELECT 2000 + value, 'excess', '0000' FROM pg_catalog.generate_series(1, $1) AS series(value)",
        fixture.schema
    ))
    .bind(extra)
    .execute(&fixture.admin)
    .await
    .expect("oversize ledger row count");
    reject_oversized_ledger(&fixture).await;
    fixture.close().await;
}

#[tokio::test]
async fn postgres17_ledger_preflight_catalog_hostility_and_locks() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url).await;
    let pins = fixture.pins().await;
    // Without an explicit trailing pg_temp, this user-defined type shadows
    // the unqualified built-in name even when pg_catalog is in search_path.
    let mut pooled = fixture.reader.acquire().await.expect("runtime session");
    sqlx::query("CREATE TEMP TABLE temp_namespace (id integer)")
        .execute(&mut *pooled)
        .await
        .expect("create temporary namespace");
    sqlx::query("CREATE TYPE pg_temp.oid AS ENUM ('shadow')")
        .execute(&mut *pooled)
        .await
        .expect("shadow oid type");
    drop(pooled);
    let db = fixture.database();
    let mut tx = db.begin().await.expect("transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await
    .expect("read-only boundary");
    tx.execute("SET LOCAL search_path = public, pg_catalog", ())
        .await
        .expect("set path");
    let mut proof = acquire_dark_shared_migration_ledger_v1023(
        &mut tx,
        &fixture.schema,
        pins,
        CUSTOM_RESTORE_PATH,
    )
    .await
    .expect("valid committed V1023 prefix");
    let mut rows = proof
        .transaction()
        .query("SELECT pg_catalog.current_setting('search_path')", ())
        .await
        .expect("proof remains on transaction");
    assert_eq!(
        rows.next()
            .await
            .expect("path row")
            .expect("path")
            .get::<String>(0)
            .expect("path"),
        CUSTOM_RESTORE_PATH
    );
    drop(rows);
    let mut other = fixture.admin.begin().await.expect("competing owner");
    let exclusive: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
        .bind(MIGRATION_LEDGER_ADVISORY_LOCK_KEY)
        .fetch_one(&mut *other)
        .await
        .expect("try exclusive owner lock");
    assert!(!exclusive, "proof must hold the shared advisory lock");
    other.rollback().await.expect("end competitor");
    drop(proof);
    tx.rollback().await.expect("release shared locks");
    let mut other = fixture.admin.begin().await.expect("next owner");
    let exclusive: bool = sqlx::query_scalar("SELECT pg_catalog.pg_try_advisory_xact_lock($1)")
        .bind(MIGRATION_LEDGER_ADVISORY_LOCK_KEY)
        .fetch_one(&mut *other)
        .await
        .expect("retry exclusive lock");
    assert!(exclusive, "rollback must release the shared lock");
    other.rollback().await.expect("end owner");

    for (predicate, missing) in [("version = 1023", 1023), ("version < 1000", 1)] {
        sqlx::query(&format!(
            "DELETE FROM {}._migrations WHERE {predicate}",
            fixture.schema
        ))
        .execute(&fixture.admin)
        .await
        .expect("remove committed prefix rows");
        let pins = fixture.pins().await;
        let mut tx = db.begin().await.expect("missing prefix transaction");
        tx.execute(
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
            (),
        )
        .await
        .expect("boundary");
        assert!(matches!(
            acquire_dark_shared_migration_ledger_v1023(&mut tx, &fixture.schema, pins, RESTORE_PATH).await,
            Err(DarkLedgerPreflightError::Database(DatabaseError::MigrationLedger(
                MigrationLedgerError::PendingMigration { version }
            ))) if version == missing
        ));
        tx.rollback().await.expect("missing prefix rollback");
        fixture.reset_ledger().await;
    }
    let pins = fixture.pins().await;

    // A valid relation with a damaged prefix cannot be repaired by this path.
    sqlx::query(&format!(
        "UPDATE {}._migrations SET checksum='bad' WHERE version=1023",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("damage checksum");
    let mut tx = db.begin().await.expect("checksum transaction");
    tx.execute(
        "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
        (),
    )
    .await
    .expect("boundary");
    assert!(matches!(
        acquire_dark_shared_migration_ledger_v1023(&mut tx, &fixture.schema, pins, RESTORE_PATH)
            .await,
        Err(DarkLedgerPreflightError::Database(_))
    ));
    tx.rollback().await.expect("checksum rollback");
    let checksum: String = sqlx::query_scalar(&format!(
        "SELECT checksum FROM {}._migrations WHERE version=1023",
        fixture.schema
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("checksum unchanged");
    assert_eq!(checksum, "bad");
    fixture.reset_ledger().await;

    // Reject a child before issuing LOCK (and before reading inherited rows).
    sqlx::query(&format!(
        "CREATE TABLE {}.child () INHERITS ({}._migrations)",
        fixture.schema, fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("hostile child");
    reject_without_relation_lock(&fixture, fixture.pins().await).await;
    sqlx::query(&format!("DROP TABLE {}.child", fixture.schema))
        .execute(&fixture.admin)
        .await
        .expect("drop child");

    sqlx::query(&format!(
        "CREATE INDEX hostile_expression ON {}._migrations ((length(description))) WHERE checksum IS NOT NULL",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("create expression and partial index");
    reject_without_relation_lock(&fixture, fixture.pins().await).await;
    sqlx::query(&format!("DROP INDEX {}.hostile_expression", fixture.schema))
        .execute(&fixture.admin)
        .await
        .expect("drop hostile index");

    sqlx::query(&format!("CREATE FUNCTION {}.trigger_fn() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RETURN NEW; END $$", fixture.schema))
        .execute(&fixture.admin).await.expect("trigger function");
    sqlx::query(&format!("CREATE TRIGGER hostile_trigger BEFORE INSERT ON {}._migrations FOR EACH ROW EXECUTE FUNCTION {}.trigger_fn()", fixture.schema, fixture.schema))
        .execute(&fixture.admin).await.expect("create trigger");
    reject_without_relation_lock(&fixture, fixture.pins().await).await;
    sqlx::query(&format!(
        "DROP TRIGGER hostile_trigger ON {}._migrations",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("drop trigger");

    // Reject PUBLIC grants, column grants, RLS and rules individually.
    for (mutation, reversal) in [
        (
            format!("GRANT SELECT ON {}._migrations TO PUBLIC", fixture.schema),
            format!(
                "REVOKE SELECT ON {}._migrations FROM PUBLIC",
                fixture.schema
            ),
        ),
        (
            format!(
                "GRANT SELECT (description) ON {}._migrations TO {}",
                fixture.schema, fixture.runtime
            ),
            format!(
                "REVOKE SELECT (description) ON {}._migrations FROM {}",
                fixture.schema, fixture.runtime
            ),
        ),
        (
            format!(
                "ALTER TABLE {}._migrations ENABLE ROW LEVEL SECURITY",
                fixture.schema
            ),
            format!(
                "ALTER TABLE {}._migrations DISABLE ROW LEVEL SECURITY",
                fixture.schema
            ),
        ),
        (
            format!(
                "CREATE RULE ledger_rewrite AS ON UPDATE TO {}._migrations DO INSTEAD NOTHING",
                fixture.schema
            ),
            format!("DROP RULE ledger_rewrite ON {}._migrations", fixture.schema),
        ),
    ] {
        sqlx::query(&mutation)
            .execute(&fixture.admin)
            .await
            .expect("mutate catalog");
        reject_without_relation_lock(&fixture, fixture.pins().await).await;
        sqlx::query(&reversal)
            .execute(&fixture.admin)
            .await
            .expect("restore catalog");
    }

    // A fake view and an attacker-owned type/cast are rejected from catalogs;
    // the side-effecting function is a positive-control read witness.
    sqlx::query(&format!(
        "CREATE TABLE {}.canary (touched boolean)",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("canary");
    sqlx::query(&format!("CREATE FUNCTION {}.trap() RETURNS bigint LANGUAGE plpgsql SECURITY DEFINER AS $$ BEGIN INSERT INTO {}.canary VALUES (true); RETURN 1; END $$", fixture.schema, fixture.schema)).execute(&fixture.admin).await.expect("trap");
    sqlx::query(&format!("SELECT {}.trap()", fixture.schema))
        .execute(&fixture.admin)
        .await
        .expect("positive control");
    let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {}.canary", fixture.schema))
        .fetch_one(&fixture.admin)
        .await
        .expect("canary read");
    assert_eq!(count, 1);
    sqlx::query(&format!("TRUNCATE {}.canary", fixture.schema))
        .execute(&fixture.admin)
        .await
        .expect("reset canary");
    sqlx::query(&format!("DROP TABLE {}._migrations", fixture.schema))
        .execute(&fixture.admin)
        .await
        .expect("drop ledger");
    sqlx::query(&format!("CREATE VIEW {}._migrations AS SELECT {}.trap() AS version, 'x'::text AS description, 'x'::text AS checksum, now() AS applied_at", fixture.schema, fixture.schema)).execute(&fixture.admin).await.expect("hostile view");
    sqlx::query(&format!(
        "ALTER VIEW {}._migrations OWNER TO {}",
        fixture.schema, fixture.owner
    ))
    .execute(&fixture.admin)
    .await
    .expect("match view owner");
    sqlx::query(&format!(
        "GRANT SELECT ON {}._migrations TO {}",
        fixture.schema, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant view read");
    reject_without_relation_lock(&fixture, fixture.pins().await).await;
    sqlx::query(&format!("DROP VIEW {}._migrations", fixture.schema))
        .execute(&fixture.admin)
        .await
        .expect("drop view");
    sqlx::query(&format!(
        "CREATE TYPE {}.hostile AS ENUM ('x')",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("hostile type");
    sqlx::query(&format!("CREATE FUNCTION {}.hostile_cast({}.hostile) RETURNS text LANGUAGE plpgsql SECURITY DEFINER AS $$ BEGIN INSERT INTO {}.canary VALUES (true); RETURN 'x'; END $$", fixture.schema, fixture.schema, fixture.schema)).execute(&fixture.admin).await.expect("hostile cast function");
    sqlx::query(&format!(
        "CREATE CAST ({}.hostile AS text) WITH FUNCTION {}.hostile_cast({}.hostile) AS IMPLICIT",
        fixture.schema, fixture.schema, fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("hostile cast");
    sqlx::query(&format!("SELECT 'x'::{}.hostile::text", fixture.schema))
        .execute(&fixture.admin)
        .await
        .expect("cast positive control");
    let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {}.canary", fixture.schema))
        .fetch_one(&fixture.admin)
        .await
        .expect("cast canary read");
    assert_eq!(count, 1, "hostile cast positive control must execute");
    sqlx::query(&format!("TRUNCATE {}.canary", fixture.schema))
        .execute(&fixture.admin)
        .await
        .expect("reset cast canary");
    sqlx::query(&format!("CREATE TABLE {}._migrations (version bigint PRIMARY KEY, description {}.hostile NOT NULL, checksum text, applied_at timestamptz NOT NULL)", fixture.schema, fixture.schema)).execute(&fixture.admin).await.expect("hostile type ledger");
    sqlx::query(&format!(
        "ALTER TABLE {}._migrations OWNER TO {}",
        fixture.schema, fixture.owner
    ))
    .execute(&fixture.admin)
    .await
    .expect("match table owner");
    sqlx::query(&format!(
        "GRANT SELECT ON {}._migrations TO {}",
        fixture.schema, fixture.runtime
    ))
    .execute(&fixture.admin)
    .await
    .expect("grant table read");
    reject_without_relation_lock(&fixture, fixture.pins().await).await;
    let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {}.canary", fixture.schema))
        .fetch_one(&fixture.admin)
        .await
        .expect("canary read");
    assert_eq!(count, 0, "view and cast were never evaluated");
    fixture.close().await;
}

#[tokio::test]
async fn postgres17_ledger_preflight_rechecks_after_wait_and_pool_rollback() {
    let Some(url) = waddle_ws_test_support::require_test_postgres_url() else {
        return;
    };
    let fixture = Fixture::new(&url).await;
    let pins = fixture.pins().await;
    sqlx::query(&format!(
        "CREATE TABLE {}.canary (touched boolean)",
        fixture.schema
    ))
    .execute(&fixture.admin)
    .await
    .expect("replacement canary");
    sqlx::query(&format!("CREATE FUNCTION {}.trap() RETURNS bigint LANGUAGE plpgsql SECURITY DEFINER AS $$ BEGIN INSERT INTO {}.canary VALUES (true); RETURN 1023; END $$", fixture.schema, fixture.schema))
        .execute(&fixture.admin).await.expect("replacement trap");
    let mut owner = fixture
        .admin
        .begin()
        .await
        .expect("replacement transaction");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {}._migrations IN ACCESS EXCLUSIVE MODE",
        fixture.schema
    ))
    .execute(&mut *owner)
    .await
    .expect("hold lock");

    let db = fixture.database();
    let schema = fixture.schema.clone();
    let task = tokio::spawn(async move {
        let mut tx = db.begin().await.expect("reader transaction");
        tx.execute(
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
            (),
        )
        .await
        .expect("boundary");
        tx.execute("SET LOCAL lock_timeout = '3s'", ())
            .await
            .expect("bounded lock");
        let result =
            acquire_dark_shared_migration_ledger_v1023(&mut tx, &schema, pins, RESTORE_PATH).await;
        let rejected = result.is_err();
        drop(result);
        tx.rollback().await.expect("rollback reader");
        rejected
    });
    timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar("SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_locks WHERE locktype='relation' AND relation=$1 AND mode='AccessShareLock' AND NOT granted)")
                .bind(Oid(pins.ledger_relation_oid)).fetch_one(&fixture.admin).await.expect("waiting reader");
            if waiting { break; }
            sleep(Duration::from_millis(10)).await;
        }
    }).await.expect("reader reached post-catalog relation wait");
    sqlx::query(&format!("DROP TABLE {}._migrations", fixture.schema))
        .execute(&mut *owner)
        .await
        .expect("drop pinned ledger");
    sqlx::query(&format!("CREATE VIEW {}._migrations AS SELECT {}.trap() AS version, 'hostile'::text AS description, 'bad'::text AS checksum, now() AS applied_at", fixture.schema, fixture.schema)).execute(&mut *owner).await.expect("replace ledger with view");
    owner.commit().await.expect("commit replacement");
    assert!(
        timeout(Duration::from_secs(5), task)
            .await
            .expect("reader completion")
            .expect("reader task"),
        "replacement after wait must fail"
    );
    let count: i64 = sqlx::query_scalar(&format!("SELECT count(*) FROM {}.canary", fixture.schema))
        .fetch_one(&fixture.admin)
        .await
        .expect("replacement canary read");
    assert_eq!(count, 0, "replacement view must not be read");
    let value: i32 = sqlx::query_scalar("SELECT 1")
        .fetch_one(&fixture.reader)
        .await
        .expect("one-slot reader pool usable after rollback");
    assert_eq!(value, 1);
    fixture.close().await;
}
