//! Actual PG17 schedules for the fixture-only, rollback-only transaction entry.

use std::time::Duration;

use sqlx::postgres::types::Oid;
use sqlx::postgres::PgPool;
use tokio::sync::oneshot;
use waddle_server::db::physical_attestation::{
    attested_pool, PgRoleName, PhysicalAttestationConfig, RestrictedIngressProfileObservation,
    RestrictedTransactionProbe,
};
use waddle_server::db::Database;
use waddle_server::test_support::database_from_attested_postgres_pool;

use super::super::Fixture;

fn database(fixture: &Fixture, config: PhysicalAttestationConfig, application: &str) -> Database {
    let pool = attested_pool(
        fixture
            .runtime_options
            .clone()
            .application_name(application),
        config,
        2,
        Duration::from_secs(1),
    )
    .expect("fixture attested pool");
    database_from_attested_postgres_pool(application, pool)
}

fn probe(
    database: &Database,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
) -> RestrictedTransactionProbe {
    RestrictedTransactionProbe::new(database.clone(), database, api.clone(), expected.clone())
        .expect("exact fixture factory")
}

pub(super) async fn exercise(
    fixture: &Fixture,
    config: &PhysicalAttestationConfig,
    api: &PgRoleName,
    epoch_oid: u32,
) {
    // Pin directly from trusted owner setup, independently of the runtime
    // observer's receipt. Both role OIDs are checked again in the transaction.
    let runtime_oid: Oid = sqlx::query_scalar("SELECT oid FROM pg_roles WHERE rolname=$1")
        .bind(&fixture.runtime)
        .fetch_one(&fixture.admin)
        .await
        .expect("runtime OID");
    let api_oid: Oid = sqlx::query_scalar("SELECT oid FROM pg_roles WHERE rolname=$1")
        .bind(api.as_str())
        .fetch_one(&fixture.admin)
        .await
        .expect("API OID");
    let expected = RestrictedIngressProfileObservation {
        manifest_version: 1,
        schema_oid: fixture
            .lineage
            .postgres_identity
            .as_ref()
            .expect("PG identity")
            .schema
            .oid,
        epoch_relation_oid: epoch_oid,
        runtime_role_oid: runtime_oid.0,
        api_owner_oid: api_oid.0,
    };
    let db = database(fixture, config.clone(), "restricted-transaction-probe");
    let good = probe(&db, api, &expected)
        .inspect()
        .await
        .expect("fresh RC complete profile");
    assert_eq!(good.profile, expected);
    let other = database(fixture, config.clone(), "same-config-different-pool");
    assert!(RestrictedTransactionProbe::new(other, &db, api.clone(), expected.clone()).is_err());
    for field in ["schema", "epoch", "runtime", "API", "version"] {
        let mut wrong = expected.clone();
        match field {
            "schema" => wrong.schema_oid += 1,
            "epoch" => wrong.epoch_relation_oid += 1,
            "runtime" => wrong.runtime_role_oid += 1,
            "API" => wrong.api_owner_oid += 1,
            "version" => wrong.manifest_version += 1,
            _ => unreachable!(),
        }
        match RestrictedTransactionProbe::new(db.clone(), &db, api.clone(), wrong) {
            Err(_) => assert!(matches!(field, "schema" | "version")),
            Ok(wrong) => assert!(wrong.inspect().await.is_err(), "wrong {field} OID"),
        }
    }
    assert!(
        probe(
            &db,
            &PgRoleName::new(&fixture.runtime).expect("runtime name"),
            &expected
        )
        .inspect()
        .await
        .is_err(),
        "wrong API role"
    );
    let mut wrong_lineage = fixture.lineage.clone();
    wrong_lineage
        .postgres_identity
        .as_mut()
        .expect("PG identity")
        .system_identifier
        .0 += 1;
    let wrong_physical = database(
        fixture,
        fixture.with_lineage(wrong_lineage),
        "wrong-physical-probe",
    );
    assert!(probe(&wrong_physical, api, &expected)
        .inspect()
        .await
        .is_err());

    let mut wrong_database = fixture.lineage.clone();
    wrong_database
        .postgres_identity
        .as_mut()
        .expect("PG identity")
        .database
        .oid += 1;
    let wrong_database = database(
        fixture,
        fixture.with_lineage(wrong_database),
        "wrong-database-probe",
    );
    assert!(probe(&wrong_database, api, &expected)
        .inspect()
        .await
        .is_err());
    let mut wrong_schema = fixture.lineage.clone();
    wrong_schema
        .postgres_identity
        .as_mut()
        .expect("PG identity")
        .schema
        .name
        .0 = "public".to_owned();
    let wrong_schema = database(
        fixture,
        fixture.with_lineage(wrong_schema),
        "wrong-schema-probe",
    );
    assert!(probe(&wrong_schema, api, &expected)
        .inspect()
        .await
        .is_err());
    assert!(
        attested_pool(
            fixture.runtime_options.clone().username(&fixture.outsider),
            config.clone(),
            1,
            Duration::from_secs(1),
        )
        .is_err(),
        "wrong configured runtime login"
    );

    concurrent_wrappers(fixture, &db, api, &expected).await;
    waited_drift(fixture, &db, api, &expected).await;
    cancelled_wrapper(fixture, &db, api, &expected).await;
    cancelled_and_timed_out_waiters(fixture, &db, api, &expected).await;

    // A public-first retained search path is refused by the pool hook; a
    // subsequent checkout must still produce a clean probe.
    let mut retained_one = db
        .begin()
        .await
        .expect("first retained fixture transaction");
    let mut retained_two = db
        .begin()
        .await
        .expect("second retained fixture transaction");
    for retained in [&mut retained_one, &mut retained_two] {
        retained
            .execute("SET search_path=public,pg_catalog", ())
            .await
            .expect("retained path");
    }
    retained_one
        .commit()
        .await
        .expect("persist first fixture defaults");
    retained_two
        .commit()
        .await
        .expect("persist second fixture defaults");
    probe(&db, api, &expected)
        .inspect()
        .await
        .expect("fresh RC after poisoned search path");

    // Keep the application path intact to exercise a reused backend whose
    // session default is RR. One connection makes the backend identity exact.
    let single_pool = attested_pool(
        fixture
            .runtime_options
            .clone()
            .application_name("retained-rr-probe"),
        config.clone(),
        1,
        Duration::from_secs(1),
    )
    .expect("single-connection attested pool");
    let single = database_from_attested_postgres_pool("retained-rr-probe", single_pool);
    let mut retained_rr = single.begin().await.expect("retained RR setup");
    let retained_pid: i64 = retained_rr
        .query("SELECT pg_catalog.pg_backend_pid()", ())
        .await
        .expect("retained backend PID")
        .next()
        .await
        .expect("PID row")
        .expect("PID value")
        .get(0)
        .expect("integer PID");
    retained_rr
        .execute(
            "SET SESSION default_transaction_isolation='repeatable read'",
            (),
        )
        .await
        .expect("retained RR default");
    retained_rr.commit().await.expect("persist RR default");
    let same_backend = probe(&single, api, &expected)
        .inspect()
        .await
        .expect("fresh RC despite retained RR default");
    assert_eq!(i64::from(same_backend.backend_pid), retained_pid);

    // Unsupported epoch is rejected without manufacturing a proof GUC.
    let qualified = format!("{}.ingress_protocol_epoch", fixture.target);
    let original: (i64, Option<String>, Option<String>) = sqlx::query_as(&format!(
        "SELECT epoch, activated_at::text, lineage_uuid::text FROM {qualified} WHERE id=1",
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("original epoch and metadata");
    mutate_epoch(
        fixture,
        2147483647,
        Some("2026-01-01 00:00:00+00"),
        Some(&fixture.lineage.lineage_uuid.0.to_string()),
    )
    .await;
    assert!(
        probe(&db, api, &expected).inspect().await.is_err(),
        "unsupported epoch"
    );
    mutate_epoch(
        fixture,
        original.0,
        original.1.as_deref(),
        original.2.as_deref(),
    )
    .await;
    probe(&db, api, &expected)
        .inspect()
        .await
        .expect("restored epoch");

    let fresh = database(fixture, config.clone(), "reconnected-restricted-probe");
    probe(&fresh, api, &expected)
        .inspect()
        .await
        .expect("new physical connection/profile");

    replacement_epoch_view(fixture, &db, api, &expected).await;
}

async fn concurrent_wrappers(
    fixture: &Fixture,
    db: &Database,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
) {
    let one = probe(db, api, expected);
    let two = probe(db, api, expected);
    let (ready_one, received_one) = oneshot::channel();
    let (ready_two, received_two) = oneshot::channel();
    let (release_one, wait_one) = oneshot::channel();
    let (release_two, wait_two) = oneshot::channel();
    let first = tokio::spawn(async move { one.inspect_paused(ready_one, wait_one).await });
    let second = tokio::spawn(async move { two.inspect_paused(ready_two, wait_two).await });
    let pid_one = received_one.await.expect("first complete profile");
    let pid_two = received_two.await.expect("second complete profile");
    assert_ne!(
        pid_one, pid_two,
        "independent concurrent physical transactions"
    );
    for pid in [pid_one, pid_two] {
        let held: (i64, bool) = sqlx::query_as(
            "SELECT count(*) FILTER (WHERE locktype='advisory' AND mode='ShareLock' AND granted), \
             bool_or(relation=$2 AND mode='AccessShareLock' AND granted) \
             FROM pg_catalog.pg_locks WHERE pid=$1",
        )
        .bind(pid)
        .bind(Oid(expected.epoch_relation_oid))
        .fetch_one(&fixture.admin)
        .await
        .expect("concurrent shared locks");
        assert_eq!(held, (2, true));
    }
    release_one.send(()).expect("release first");
    release_two.send(()).expect("release second");
    first
        .await
        .expect("first task")
        .expect("first acknowledged rollback");
    second
        .await
        .expect("second task")
        .expect("second acknowledged rollback");
}

async fn epoch_waiter(admin: &PgPool, epoch_oid: u32) -> i32 {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let pid: Option<i32> = sqlx::query_scalar(
                "SELECT a.pid FROM pg_catalog.pg_stat_activity a \
                 JOIN pg_catalog.pg_locks l ON l.pid=a.pid \
                 WHERE a.application_name='restricted-transaction-probe' \
                   AND l.relation=$1 AND l.mode='AccessShareLock' AND NOT l.granted",
            )
            .bind(Oid(epoch_oid))
            .fetch_optional(admin)
            .await
            .expect("actual AS waiter");
            if let Some(pid) = pid {
                break pid;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("epoch AS waiter observed")
}

async fn waited_drift(
    fixture: &Fixture,
    db: &Database,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
) {
    let s = &fixture.target;
    let function = format!("{s}.waddle_ingress_message_is_terminal(uuid)");
    let definition: String = sqlx::query_scalar("SELECT pg_get_functiondef($1::regprocedure)")
        .bind(&function)
        .fetch_one(&fixture.admin)
        .await
        .expect("exact original function");
    let restore = format!("{definition}; ALTER FUNCTION {function} OWNER TO {}; REVOKE ALL ON FUNCTION {function} FROM PUBLIC; GRANT EXECUTE ON FUNCTION {function} TO {}", api.as_str(),fixture.runtime);
    for mutation in [
        format!("CREATE OR REPLACE FUNCTION {s}.waddle_ingress_message_is_terminal(p_message_key uuid) RETURNS boolean LANGUAGE plpgsql AS 'BEGIN RETURN true; END'"),
        format!("REVOKE EXECUTE ON FUNCTION {function} FROM {}",fixture.runtime),
    ] {
        let mut owner = fixture.admin.begin().await.expect("AX mutation transaction");
        sqlx::query(&format!("LOCK TABLE ONLY {s}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE"))
            .execute(&mut *owner).await.expect("hold owner AX");
        let candidate = probe(db, api, expected);
        let waiting = tokio::spawn(async move { candidate.inspect().await });
        let pid = epoch_waiter(&fixture.admin, expected.epoch_relation_oid).await;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_catalog.pg_locks WHERE pid=$1 AND locktype='advisory' AND mode='ShareLock' AND granted",
        ).bind(pid).fetch_one(&fixture.admin).await.expect("ordered shared locks before AS wait");
        assert_eq!(count, 2);
        sqlx::raw_sql(&mutation).execute(&mut *owner).await.expect("same-epoch hostile mutation");
        owner.commit().await.expect("publish drift and release AX");
        assert!(waiting.await.expect("waiter task").is_err(), "fresh profile must reject drift after AX wait");
        super::execute(&fixture.admin, &restore).await;
        probe(db, api, expected).inspect().await.expect("restored complete profile");
    }
}

async fn replacement_epoch_view(
    fixture: &Fixture,
    db: &Database,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
) {
    let s = &fixture.target;
    let mut owner = fixture.admin.begin().await.expect("held epoch AX");
    sqlx::query(&format!(
        "LOCK TABLE ONLY {s}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE"
    ))
    .execute(&mut *owner)
    .await
    .expect("block probe at epoch AS");
    let candidate = probe(db, api, expected);
    let waiting = tokio::spawn(async move { candidate.inspect().await });
    epoch_waiter(&fixture.admin, expected.epoch_relation_oid).await;

    // A READ ONLY transaction can still run pg_advisory_lock from a view.
    // The replacement retains the name but changes the trusted relation OID
    // and kind; a pre-verification epoch SELECT would leave a session lock.
    sqlx::raw_sql(&format!(
        "ALTER TABLE {s}.ingress_protocol_epoch RENAME TO ingress_protocol_epoch_original; \
         CREATE VIEW {s}.ingress_protocol_epoch AS \
           SELECT id, epoch + CASE WHEN pg_catalog.pg_advisory_lock(987654321) IS NULL \
             THEN 0 ELSE 1 END AS epoch \
           FROM {s}.ingress_protocol_epoch_original; \
         GRANT SELECT ON {s}.ingress_protocol_epoch TO {}",
        fixture.runtime,
    ))
    .execute(&mut *owner)
    .await
    .expect("replace epoch name with volatile view");
    owner
        .commit()
        .await
        .expect("publish replacement after AS wait");
    assert!(
        waiting.await.expect("view replacement waiter").is_err(),
        "the pinned epoch relation was replaced"
    );
    let leaked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM pg_catalog.pg_locks l \
         JOIN pg_catalog.pg_stat_activity a ON a.pid=l.pid \
         WHERE a.datname=$1 AND a.application_name='restricted-transaction-probe' \
           AND l.locktype='advisory' AND l.mode='ExclusiveLock' AND l.granted",
    )
    .bind(&fixture.database_name)
    .fetch_one(&fixture.admin)
    .await
    .expect("inspect session advisory locks");
    assert_eq!(leaked, 0, "rejected view must never invoke its function");
    sqlx::raw_sql(&format!(
        "DROP VIEW {s}.ingress_protocol_epoch; \
         ALTER TABLE {s}.ingress_protocol_epoch_original RENAME TO ingress_protocol_epoch",
    ))
    .execute(&fixture.admin)
    .await
    .expect("restore pinned epoch relation");
    probe(db, api, expected)
        .inspect()
        .await
        .expect("restored exact epoch relation");
}

async fn cancelled_wrapper(
    fixture: &Fixture,
    db: &Database,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
) {
    let candidate = probe(db, api, expected);
    let (ready, received) = oneshot::channel();
    let (_release, wait) = oneshot::channel();
    let task = tokio::spawn(async move { candidate.inspect_paused(ready, wait).await });
    let pid = received.await.expect("profile checked before cancellation");
    task.abort();
    assert!(task.await.expect_err("cancelled task").is_cancelled());
    // Task cancellation is not rollback proof. Force another checkout to
    // process the queued rollback, then observe the original backend's actual
    // transaction/lock state before asserting release.
    probe(db, api, expected)
        .inspect()
        .await
        .expect("checkout after cancellation");
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let open: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1 AND xact_start IS NOT NULL) \
                 OR EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 AND locktype='advisory')",
            ).bind(pid).fetch_one(&fixture.admin).await.expect("observe backend rollback");
            if !open { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("server-side rollback and lock release observed");
}

async fn cancelled_and_timed_out_waiters(
    fixture: &Fixture,
    db: &Database,
    api: &PgRoleName,
    expected: &RestrictedIngressProfileObservation,
) {
    for cancel in [true, false] {
        let mut owner = fixture.admin.begin().await.expect("held epoch AX");
        sqlx::query(&format!(
            "LOCK TABLE ONLY {}.ingress_protocol_epoch IN ACCESS EXCLUSIVE MODE",
            fixture.target,
        ))
        .execute(&mut *owner)
        .await
        .expect("block probe AS");
        let candidate = probe(db, api, expected);
        let task = tokio::spawn(async move { candidate.inspect().await });
        let pid = epoch_waiter(&fixture.admin, expected.epoch_relation_oid).await;
        if cancel {
            task.abort();
            assert!(task
                .await
                .expect_err("cancelled waiting probe")
                .is_cancelled());
        } else {
            assert!(tokio::time::timeout(Duration::from_secs(5), task)
                .await
                .expect("bounded lock wait")
                .expect("timeout task")
                .is_err());
        }
        owner
            .rollback()
            .await
            .expect("release AX after cancellation/refusal");
        probe(db, api, expected)
            .inspect()
            .await
            .expect("fresh probe after waiting refusal");
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let open: bool = sqlx::query_scalar(
                    "SELECT EXISTS(SELECT 1 FROM pg_catalog.pg_stat_activity WHERE pid=$1 AND xact_start IS NOT NULL) \
                     OR EXISTS(SELECT 1 FROM pg_catalog.pg_locks WHERE pid=$1 AND locktype='advisory')",
                ).bind(pid).fetch_one(&fixture.admin).await.expect("waiting backend release");
                if !open { break; }
                tokio::task::yield_now().await;
            }
        }).await.expect("original waiting transaction actually ended");
    }
}

async fn mutate_epoch(
    fixture: &Fixture,
    value: i64,
    activated: Option<&str>,
    lineage: Option<&str>,
) {
    let qualified = format!("{}.ingress_protocol_epoch", fixture.target);
    let mut owner = fixture.admin.begin().await.expect("epoch fixture mutation");
    sqlx::query(&format!("ALTER TABLE {qualified} DISABLE TRIGGER USER"))
        .execute(&mut *owner)
        .await
        .expect("disable monotonic fixture triggers");
    sqlx::query(&format!("UPDATE {qualified} SET epoch=$1, activated_at=$2::timestamptz, lineage_uuid=$3::uuid WHERE id=1"))
        .bind(value).bind(activated).bind(lineage).execute(&mut *owner).await.expect("fixture epoch contents");
    for trigger in [
        "ingress_protocol_epoch_guard_row",
        "ingress_protocol_epoch_guard_truncate",
    ] {
        sqlx::query(&format!(
            "ALTER TABLE {qualified} ENABLE ALWAYS TRIGGER {trigger}"
        ))
        .execute(&mut *owner)
        .await
        .expect("restore exact trigger mode");
    }
    owner
        .commit()
        .await
        .expect("publish fixture epoch mutation");
}
