use super::super::super::{
    proxy_runtime_options, test_url, typed_bounds, wait_for_blocked_application, Fixture,
    CATALOG_GRANT_LOCK,
};
use super::super::{
    direct_pin, postgres_fixture, seed_pg_direct, seed_sqlite_direct, sqlite_count, sqlite_exec,
    sqlite_fixture,
};
use super::{installed_v1009, pg_attempt, request, sqlite_attempt};
use std::sync::Arc;
use std::time::Duration;

use sqlx::{Connection, PgConnection};
use waddle_server::db::physical_attestation::{
    allocation_lab_postgres_ddl, DarkIngressAuthorityError, DirectAdmissionLab,
    DirectAdmissionLimits, FixtureAllocationError, FixtureConnection, FixtureFrame, FixtureStream,
    PhysicalAttestationConfig, ALLOCATION_LAB_SQLITE_DDL,
};
use waddle_server::db::Database;
use waddle_xmpp::Stanza;

fn other(body: &str) -> Stanza {
    let mut message = xmpp_parsers::message::Message::new(None);
    message
        .bodies
        .insert(xmpp_parsers::message::Lang::new(), body.to_owned());
    Stanza::Message(message)
}

fn stream(connection: &FixtureConnection) -> FixtureStream {
    FixtureStream {
        id: uuid::Uuid::new_v4(),
        connection: connection.clone(),
    }
}

async fn install_sqlite(db: &Database) {
    let mut tx = db.begin_immediate().await.expect("fixture DDL transaction");
    tx.execute_batch(ALLOCATION_LAB_SQLITE_DDL)
        .await
        .expect("unregistered fixture DDL");
    tx.commit().await.expect("commit DDL");
}

async fn install_pg(fixture: &Fixture, config: &PhysicalAttestationConfig) {
    sqlx::raw_sql(&allocation_lab_postgres_ddl(config))
        .execute(&fixture.admin)
        .await
        .expect("unregistered fixture DDL");
    for table in [
        "direct_fixture_registry",
        "direct_fixture_stream",
        "direct_fixture_capability",
        "direct_fixture_slot",
        "direct_fixture_resolution",
    ] {
        sqlx::query(&format!(
            "GRANT SELECT, INSERT, UPDATE, DELETE ON {}.{table} TO {}",
            fixture.target, fixture.runtime
        ))
        .execute(&fixture.admin)
        .await
        .expect("fixture grants");
    }
}

#[tokio::test]
async fn sqlite_fixture_allocation_serializes_all_frames_and_fences_recovery() {
    let (db, _) = sqlite_fixture().await;
    install_sqlite(&db).await;
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_sqlite_direct(&db, &pin, envelope, intent).await;
    let mut admission = request(pin);
    let lease = sqlite_attempt(&db, &mut admission).await;
    let lab = Arc::new(
        DirectAdmissionLab::sqlite(
            &db,
            typed_bounds(),
            admission.connection.clone(),
            DirectAdmissionLimits::new(10, 4096).expect("limits"),
        )
        .expect("lab"),
    );
    lab.start(&admission, &lease).await.expect("full admission");
    let stream = stream(&admission.connection);
    let initial_token = uuid::Uuid::new_v4();
    let (opened_a, opened_b) = tokio::join!(
        lab.open_fixture_stream(&stream, initial_token),
        lab.open_fixture_stream(&stream, initial_token),
    );
    let cap = opened_a.expect("stream");
    opened_b.expect("concurrent exact open");
    let binding = lab
        .allocate_fixture(&cap, &FixtureFrame::Admission(&admission))
        .await
        .expect("allocate admission");
    assert_eq!((binding.ordinal, binding.h), (1, 1));
    assert_eq!(
        lab.allocate_fixture(&cap, &FixtureFrame::Admission(&admission))
            .await
            .expect("exact retry"),
        binding
    );
    let mut wrong = admission.clone();
    wrong.attempt_token = uuid::Uuid::new_v4();
    assert!(matches!(
        lab.allocate_fixture(&cap, &FixtureFrame::Admission(&wrong))
            .await,
        Err(FixtureAllocationError::Conflict)
    ));
    let mut stale_connection = admission.connection.clone();
    stale_connection.generation = uuid::Uuid::new_v4();
    let stale_lab = DirectAdmissionLab::sqlite(
        &db,
        typed_bounds(),
        stale_connection,
        DirectAdmissionLimits::new(10, 4096).expect("limits"),
    )
    .expect("stale lab");
    assert!(matches!(
        stale_lab
            .allocate_fixture(&cap, &FixtureFrame::Admission(&admission))
            .await,
        Err(FixtureAllocationError::Rejected)
    ));
    let mut stale_admission = admission.clone();
    stale_admission.connection.generation = uuid::Uuid::new_v4();
    assert!(matches!(
        lab.allocate_fixture(&cap, &FixtureFrame::Admission(&stale_admission))
            .await,
        Err(FixtureAllocationError::Rejected)
    ));
    let stanza_a = other("non-ingress a");
    let stanza_b = other("non-ingress b");
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    assert!(matches!(
        lab.allocate_fixture(
            &cap,
            &FixtureFrame::Other {
                id: a,
                stanza: &stanza_a
            }
        )
        .await,
        Err(FixtureAllocationError::Unresolved)
    ));
    let mut forged = binding.clone();
    forged.h = 2;
    assert!(matches!(
        lab.reconcile_fixture_slot(&cap, &forged).await,
        Err(FixtureAllocationError::Rejected)
    ));
    lab.reconcile_fixture_slot(&cap, &binding)
        .await
        .expect("synthetic reconciliation");
    lab.reconcile_fixture_slot(&cap, &binding)
        .await
        .expect("exact reconciliation retry");
    let left = {
        let lab = lab.clone();
        let cap = cap.clone();
        let stanza = stanza_a.clone();
        tokio::spawn(async move {
            lab.allocate_fixture(
                &cap,
                &FixtureFrame::Other {
                    id: a,
                    stanza: &stanza,
                },
            )
            .await
        })
    };
    let right = {
        let lab = lab.clone();
        let cap = cap.clone();
        let stanza = stanza_b.clone();
        tokio::spawn(async move {
            lab.allocate_fixture(
                &cap,
                &FixtureFrame::Other {
                    id: b,
                    stanza: &stanza,
                },
            )
            .await
        })
    };
    let results = [left.await.expect("task"), right.await.expect("task")];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(FixtureAllocationError::Unresolved)))
            .count(),
        1
    );
    let winner = results.into_iter().find_map(Result::ok).expect("winner");
    assert_eq!(winner.ordinal, 2);
    let (winner_frame, loser_frame) = if winner.frame_id == a {
        (
            FixtureFrame::Other {
                id: a,
                stanza: &stanza_a,
            },
            FixtureFrame::Other {
                id: b,
                stanza: &stanza_b,
            },
        )
    } else {
        (
            FixtureFrame::Other {
                id: b,
                stanza: &stanza_b,
            },
            FixtureFrame::Other {
                id: a,
                stanza: &stanza_a,
            },
        )
    };
    let fenced = lab
        .fence_fixture_capability(&cap, uuid::Uuid::new_v4())
        .await
        .expect("fence");
    assert!(matches!(
        lab.fence_fixture_capability(fenced.capability(), initial_token)
            .await,
        Err(FixtureAllocationError::Rejected)
    ));
    assert!(matches!(
        lab.allocate_fixture(&cap, &winner_frame).await,
        Err(FixtureAllocationError::Rejected)
    ));
    assert_eq!(
        lab.lookup_fixture(&fenced, &winner_frame)
            .await
            .expect("synchronized lookup"),
        Some(winner.clone())
    );
    assert!(lab
        .lookup_fixture(&fenced, &loser_frame)
        .await
        .expect("absence after fence")
        .is_none());
    let mismatch_stanza = other("different payload");
    let mismatch = FixtureFrame::Other {
        id: winner.frame_id,
        stanza: &mismatch_stanza,
    };
    assert!(matches!(
        lab.lookup_fixture(&fenced, &mismatch).await,
        Err(FixtureAllocationError::Conflict)
    ));
    lab.reconcile_fixture_slot(fenced.capability(), &winner)
        .await
        .expect("resolve winner");
    let third = lab
        .allocate_fixture(fenced.capability(), &loser_frame)
        .await
        .expect("next slot");
    assert_eq!(third.ordinal, 3);
    lab.retire_fixture_stream(fenced.capability())
        .await
        .expect("retire unresolved stream");
    assert!(matches!(
        lab.allocate_fixture(fenced.capability(), &loser_frame)
            .await,
        Err(FixtureAllocationError::Retired)
    ));
    assert_eq!(
        sqlite_count(&db, "SELECT COUNT(*) FROM main.direct_fixture_slot").await,
        3
    );
    assert_eq!(
        sqlite_count(&db, "SELECT COUNT(*) FROM main.direct_fixture_resolution").await,
        2
    );
    let stored = lab
        .lookup(&admission)
        .await
        .expect("admission readback")
        .expect("full payload retained");
    assert_eq!(stored.pin.payload, admission.pin.payload);
    let guard = db.guard().await.expect("guard");
    for sql in ["UPDATE main.direct_fixture_slot SET payload = 'forged'", "DELETE FROM main.direct_fixture_slot", "INSERT OR REPLACE INTO main.direct_fixture_slot SELECT * FROM main.direct_fixture_slot LIMIT 1", "DELETE FROM main.direct_fixture_stream", "UPDATE main.direct_fixture_stream SET retired = 0"] {
        assert!(guard.execute(sql, ()).await.is_err(), "immutability: {sql}");
    }
}

#[tokio::test]
async fn sqlite_fixture_allocation_rollback_wrap_capacity_and_temp_shadow() {
    let (db, _) = sqlite_fixture().await;
    install_sqlite(&db).await;
    let connection =
        request(direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop").0).connection;
    let lab = DirectAdmissionLab::sqlite(
        &db,
        typed_bounds(),
        connection.clone(),
        DirectAdmissionLimits::new(2, 4096).expect("limits"),
    )
    .expect("lab");
    let stream = stream(&connection);
    let cap = lab
        .open_fixture_stream(&stream, uuid::Uuid::new_v4())
        .await
        .expect("stream");
    let stanza = other("fixture stanza");
    let id = uuid::Uuid::new_v4();
    let frame = FixtureFrame::Other {
        id,
        stanza: &stanza,
    };
    sqlite_exec(&db, "CREATE TEMP TABLE direct_fixture_stream AS SELECT * FROM main.direct_fixture_stream WHERE 0").await;
    sqlite_exec(
        &db,
        "CREATE TEMP TABLE direct_fixture_slot AS SELECT * FROM main.direct_fixture_slot WHERE 0",
    )
    .await;
    sqlite_exec(&db, "CREATE TRIGGER main.reject_fixture_counter BEFORE UPDATE OF ordinal ON direct_fixture_stream BEGIN SELECT RAISE(ABORT, 'injected'); END").await;
    assert!(lab.allocate_fixture(&cap, &frame).await.is_err());
    assert_eq!(
        sqlite_count(&db, "SELECT COUNT(*) FROM main.direct_fixture_slot").await,
        0
    );
    assert_eq!(
        sqlite_count(&db, "SELECT ordinal FROM main.direct_fixture_stream").await,
        0
    );
    sqlite_exec(&db, "DROP TRIGGER main.reject_fixture_counter").await;
    // Seed an artificial prehistory solely to exercise the 32-bit boundary.
    sqlite_exec(
        &db,
        "UPDATE main.direct_fixture_stream SET ordinal = 4294967294",
    )
    .await;
    let pre_wrap = lab
        .allocate_fixture(&cap, &frame)
        .await
        .expect("before wrap");
    assert_eq!((pre_wrap.ordinal, pre_wrap.h), (u32::MAX as u64, u32::MAX));
    lab.reconcile_fixture_slot(&cap, &pre_wrap)
        .await
        .expect("resolve");
    let second_id = uuid::Uuid::new_v4();
    let second = FixtureFrame::Other {
        id: second_id,
        stanza: &stanza,
    };
    let wrapped = lab
        .allocate_fixture(&cap, &second)
        .await
        .expect("after wrap");
    assert_eq!((wrapped.ordinal, wrapped.h), (u32::MAX as u64 + 1, 0));
    lab.reconcile_fixture_slot(&cap, &wrapped)
        .await
        .expect("resolve");
    let third = FixtureFrame::Other {
        id: uuid::Uuid::new_v4(),
        stanza: &stanza,
    };
    assert!(matches!(
        lab.allocate_fixture(&cap, &third).await,
        Err(FixtureAllocationError::Capacity)
    ));
    sqlite_exec(
        &db,
        "UPDATE main.direct_fixture_stream SET ordinal = 9223372036854775807",
    )
    .await;
    assert!(matches!(
        lab.allocate_fixture(&cap, &third).await,
        Err(FixtureAllocationError::Overflow)
    ));
    assert_eq!(
        sqlite_count(&db, "SELECT COUNT(*) FROM main.direct_fixture_slot").await,
        2
    );
    assert_eq!(
        sqlite_count(&db, "SELECT COUNT(*) FROM temp.direct_fixture_slot").await,
        0
    );
}

#[tokio::test]
async fn postgres_fixture_allocation_serializes_and_uses_attested_schema() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, _) = postgres_fixture(&fixture).await;
    install_pg(&fixture, &config).await;
    installed_v1009(&fixture).await;
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &pin, envelope, intent).await;
    let mut admission = request(pin);
    let lease = pg_attempt(&fixture, &config, &mut admission).await;
    let schema = &fixture.target;
    let shadow = format!("allocation_shadow_{}", uuid::Uuid::new_v4().simple());
    for statement in [
        format!("CREATE SCHEMA {shadow}"),
        format!("CREATE TABLE {shadow}.direct_fixture_stream (LIKE {schema}.direct_fixture_stream INCLUDING ALL)"),
        format!("CREATE TABLE {shadow}.direct_fixture_slot (LIKE {schema}.direct_fixture_slot INCLUDING ALL)"),
        format!("GRANT USAGE ON SCHEMA {shadow} TO {}", fixture.runtime),
        format!("GRANT ALL ON ALL TABLES IN SCHEMA {shadow} TO {}", fixture.runtime),
        format!("CREATE FUNCTION {schema}.poison_fixture_path() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN PERFORM pg_catalog.set_config('search_path', '{shadow}, {schema}', true); RETURN NEW; END $$"),
        format!("CREATE TRIGGER poison_fixture_path BEFORE UPDATE ON {schema}.direct_fixture_stream FOR EACH ROW EXECUTE FUNCTION {schema}.poison_fixture_path()"),
    ] { sqlx::query(&statement).execute(&fixture.admin).await.expect("hostile search path"); }
    let mut restricted = PgConnection::connect_with(&fixture.runtime_options)
        .await
        .expect("runtime role");
    assert!(
        sqlx::query("CREATE TEMP TABLE forbidden_fixture_temp (id integer)")
            .execute(&mut restricted)
            .await
            .is_err()
    );
    drop(restricted);
    let app_a = format!("allocation_a_{}", uuid::Uuid::new_v4().simple());
    let app_b = format!("allocation_b_{}", uuid::Uuid::new_v4().simple());
    let limits = DirectAdmissionLimits::new(5, 4096).expect("limits");
    let lab_a = Arc::new(
        DirectAdmissionLab::postgres(
            fixture.runtime_options.clone().application_name(&app_a),
            config.clone(),
            1,
            typed_bounds(),
            admission.connection.clone(),
            limits,
        )
        .expect("lab A"),
    );
    let lab_b = Arc::new(
        DirectAdmissionLab::postgres(
            fixture.runtime_options.clone().application_name(&app_b),
            config,
            1,
            typed_bounds(),
            admission.connection.clone(),
            limits,
        )
        .expect("lab B"),
    );
    lab_a.start(&admission, &lease).await.expect("admission");
    let stream = stream(&admission.connection);
    let initial_token = uuid::Uuid::new_v4();
    let mut registry_holder = fixture.admin.begin().await.expect("registry holder");
    sqlx::query(&format!(
        "SELECT id FROM {schema}.direct_fixture_registry WHERE id = 1 FOR UPDATE"
    ))
    .fetch_one(&mut *registry_holder)
    .await
    .expect("registry lock");
    let opening_a = {
        let lab = lab_a.clone();
        let stream = stream.clone();
        tokio::spawn(async move { lab.open_fixture_stream(&stream, initial_token).await })
    };
    let opening_b = {
        let lab = lab_b.clone();
        let stream = stream.clone();
        tokio::spawn(async move { lab.open_fixture_stream(&stream, initial_token).await })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE application_name = ANY($1) AND wait_event_type = 'Lock'")
                .bind(vec![app_a.clone(), app_b.clone()]).fetch_one(&fixture.admin).await.expect("observe open waiters");
            if waiting == 2 { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("both opens wait for registry");
    registry_holder.commit().await.expect("release registry");
    let cap = opening_a.await.expect("open task").expect("stream");
    opening_b
        .await
        .expect("open task")
        .expect("concurrent exact open");
    let first = lab_a
        .allocate_fixture(&cap, &FixtureFrame::Admission(&admission))
        .await
        .expect("admission slot");
    assert_eq!((first.ordinal, first.h), (1, 1));
    assert_eq!(
        lab_b
            .allocate_fixture(&cap, &FixtureFrame::Admission(&admission))
            .await
            .expect("replay"),
        first
    );
    let mut mismatch = admission.clone();
    mismatch.attempt_token = uuid::Uuid::new_v4();
    assert!(matches!(
        lab_a
            .allocate_fixture(&cap, &FixtureFrame::Admission(&mismatch))
            .await,
        Err(FixtureAllocationError::Conflict)
    ));
    lab_a
        .reconcile_fixture_slot(&cap, &first)
        .await
        .expect("reconcile");
    let stanza_a = other("non-ingress a");
    let stanza_b = other("non-ingress b");
    let a = uuid::Uuid::new_v4();
    let b = uuid::Uuid::new_v4();
    let mut holder = fixture.admin.begin().await.expect("holder");
    sqlx::query(&format!(
        "SELECT 1 FROM {schema}.direct_fixture_stream WHERE stream_id = $1 FOR UPDATE"
    ))
    .bind(stream.id)
    .fetch_one(&mut *holder)
    .await
    .expect("stream lock");
    let left = {
        let lab = lab_a.clone();
        let cap = cap.clone();
        let stanza = stanza_a.clone();
        tokio::spawn(async move {
            lab.allocate_fixture(
                &cap,
                &FixtureFrame::Other {
                    id: a,
                    stanza: &stanza,
                },
            )
            .await
        })
    };
    let right = {
        let lab = lab_b.clone();
        let cap = cap.clone();
        let stanza = stanza_b.clone();
        tokio::spawn(async move {
            lab.allocate_fixture(
                &cap,
                &FixtureFrame::Other {
                    id: b,
                    stanza: &stanza,
                },
            )
            .await
        })
    };
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: i64 = sqlx::query_scalar("SELECT count(*) FROM pg_catalog.pg_stat_activity WHERE application_name = ANY($1) AND wait_event_type = 'Lock'").bind(vec![app_a.clone(), app_b.clone()]).fetch_one(&fixture.admin).await.expect("observe waiters");
            if waiting == 2 { break; }
            tokio::task::yield_now().await;
        }
    }).await.expect("both allocate against one stream lock");
    holder.commit().await.expect("release");
    let results = [left.await.expect("task"), right.await.expect("task")];
    assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
    assert_eq!(
        results
            .iter()
            .filter(|r| matches!(r, Err(FixtureAllocationError::Unresolved)))
            .count(),
        1
    );
    let winner = results.into_iter().find_map(Result::ok).expect("winner");
    assert_eq!(winner.ordinal, 2);
    let (winner_frame, loser_frame) = if winner.frame_id == a {
        (
            FixtureFrame::Other {
                id: a,
                stanza: &stanza_a,
            },
            FixtureFrame::Other {
                id: b,
                stanza: &stanza_b,
            },
        )
    } else {
        (
            FixtureFrame::Other {
                id: b,
                stanza: &stanza_b,
            },
            FixtureFrame::Other {
                id: a,
                stanza: &stanza_a,
            },
        )
    };
    let next = uuid::Uuid::new_v4();
    let fenced = lab_a
        .fence_fixture_capability(&cap, next)
        .await
        .expect("fence");
    lab_b
        .fence_fixture_capability(&cap, next)
        .await
        .expect("fence retry");
    assert!(matches!(
        lab_a.allocate_fixture(&cap, &winner_frame).await,
        Err(FixtureAllocationError::Rejected)
    ));
    assert_eq!(
        lab_a
            .lookup_fixture(&fenced, &winner_frame)
            .await
            .expect("lookup"),
        Some(winner.clone())
    );
    assert!(lab_a
        .lookup_fixture(&fenced, &loser_frame)
        .await
        .expect("lookup absent")
        .is_none());
    lab_a
        .reconcile_fixture_slot(fenced.capability(), &winner)
        .await
        .expect("resolve");
    for statement in [
        format!("CREATE FUNCTION {schema}.reject_fixture_counter() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected'; END $$"),
        format!("CREATE TRIGGER reject_fixture_counter BEFORE UPDATE OF ordinal ON {schema}.direct_fixture_stream FOR EACH ROW EXECUTE FUNCTION {schema}.reject_fixture_counter()"),
    ] { sqlx::query(&statement).execute(&fixture.admin).await.expect("inject failure"); }
    assert!(lab_a
        .allocate_fixture(fenced.capability(), &loser_frame)
        .await
        .is_err());
    let (count, ordinal): (i64, i64) = sqlx::query_as(&format!("SELECT (SELECT count(*) FROM {schema}.direct_fixture_slot), ordinal FROM {schema}.direct_fixture_stream WHERE stream_id = $1")).bind(stream.id).fetch_one(&fixture.admin).await.expect("rollback readback");
    assert_eq!((count, ordinal), (2, 2));
    sqlx::query(&format!(
        "DROP TRIGGER reject_fixture_counter ON {schema}.direct_fixture_stream"
    ))
    .execute(&fixture.admin)
    .await
    .expect("remove failure");
    let third = lab_a
        .allocate_fixture(fenced.capability(), &loser_frame)
        .await
        .expect("allocate after rollback");
    assert_eq!(third.ordinal, 3);
    lab_a
        .reconcile_fixture_slot(fenced.capability(), &third)
        .await
        .expect("resolve");
    // Artificial fixture prehistory tests the 32-bit boundary and 64-bit limit.
    sqlx::query(&format!(
        "UPDATE {schema}.direct_fixture_stream SET ordinal = 4294967295 WHERE stream_id = $1"
    ))
    .bind(stream.id)
    .execute(&fixture.admin)
    .await
    .expect("seed wrap");
    let wrap_id = uuid::Uuid::new_v4();
    let wrap = FixtureFrame::Other {
        id: wrap_id,
        stanza: &stanza_a,
    };
    let binding = lab_a
        .allocate_fixture(fenced.capability(), &wrap)
        .await
        .expect("wrap");
    assert_eq!((binding.ordinal, binding.h), (4294967296, 0));
    lab_a
        .reconcile_fixture_slot(fenced.capability(), &binding)
        .await
        .expect("resolve wrap");
    sqlx::query(&format!("UPDATE {schema}.direct_fixture_stream SET ordinal = 9223372036854775807 WHERE stream_id = $1")).bind(stream.id).execute(&fixture.admin).await.expect("seed overflow");
    assert!(matches!(
        lab_a
            .allocate_fixture(
                fenced.capability(),
                &FixtureFrame::Other {
                    id: uuid::Uuid::new_v4(),
                    stanza: &stanza_a
                }
            )
            .await,
        Err(FixtureAllocationError::Overflow)
    ));
    for statement in [
        format!("UPDATE {schema}.direct_fixture_slot SET payload = 'forged'"),
        format!("DELETE FROM {schema}.direct_fixture_slot"),
        format!("TRUNCATE {schema}.direct_fixture_slot"),
    ] {
        assert!(sqlx::query(&statement)
            .execute(&fixture.admin)
            .await
            .is_err());
    }
    let shadow_rows: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {shadow}.direct_fixture_slot"
    ))
    .fetch_one(&fixture.admin)
    .await
    .expect("shadow count");
    assert_eq!(shadow_rows, 0);
    assert_eq!(
        lab_a
            .lookup(&admission)
            .await
            .expect("payload readback")
            .expect("admission")
            .pin
            .payload,
        admission.pin.payload
    );
    drop(lab_a);
    drop(lab_b);
    fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_fixture_allocation_lost_reply_and_cancel_require_fence() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let Some(url) = test_url() else { return };
    let fixture = Fixture::new(url).await;
    let (config, _) = postgres_fixture(&fixture).await;
    install_pg(&fixture, &config).await;
    installed_v1009(&fixture).await;
    let (pin, envelope, intent) = direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop");
    seed_pg_direct(&fixture, &pin, envelope, intent).await;
    let mut admission = request(pin);
    let lease = pg_attempt(&fixture, &config, &mut admission).await;
    let limits = DirectAdmissionLimits::new(10, 4096).expect("limits");
    let lab = Arc::new(
        DirectAdmissionLab::postgres(
            fixture.runtime_options.clone(),
            config.clone(),
            1,
            typed_bounds(),
            admission.connection.clone(),
            limits,
        )
        .expect("lab"),
    );
    lab.start(&admission, &lease).await.expect("admission");
    let stream = stream(&admission.connection);
    let cap = lab
        .open_fixture_stream(&stream, uuid::Uuid::new_v4())
        .await
        .expect("stream");
    let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
        .await
        .expect("proxy");
    let port = listener.local_addr().expect("address").port();
    let upstream = fixture.admin_options.clone();
    let proxy = tokio::spawn(async move {
        let (client, _) = listener.accept().await.expect("accept");
        let server = crate::postgres_proxy::connect_local_postgres(&upstream)
            .await
            .expect("upstream");
        let (mut client_read, mut client_write) = client.into_split();
        let (mut server_read, mut server_write) = tokio::io::split(server);
        let forward = tokio::spawn(async move {
            let _ = tokio::io::copy(&mut client_read, &mut server_write).await;
        });
        loop {
            let tag = server_read.read_u8().await.expect("tag");
            let length = server_read.read_u32().await.expect("length");
            assert!((4..=1_048_576).contains(&length));
            let mut body = vec![0; usize::try_from(length - 4).expect("length")];
            server_read.read_exact(&mut body).await.expect("body");
            if tag == b'C' && body == b"COMMIT\0" {
                break;
            }
            client_write.write_u8(tag).await.expect("tag");
            client_write.write_u32(length).await.expect("length");
            client_write.write_all(&body).await.expect("body");
        }
        forward.abort();
        let _ = forward.await;
        client_write.shutdown().await.expect("drop committed reply");
    });
    let proxied = DirectAdmissionLab::postgres(
        proxy_runtime_options(&fixture, port),
        config.clone(),
        1,
        typed_bounds(),
        admission.connection.clone(),
        limits,
    )
    .expect("proxied lab");
    assert!(matches!(
        proxied
            .allocate_fixture(&cap, &FixtureFrame::Admission(&admission))
            .await,
        Err(FixtureAllocationError::Authority(
            DarkIngressAuthorityError::CommitIndeterminate
        ))
    ));
    tokio::time::timeout(Duration::from_secs(5), proxy)
        .await
        .expect("proxy completes")
        .expect("proxy task");
    drop(proxied);
    let fenced = lab
        .fence_fixture_capability(&cap, uuid::Uuid::new_v4())
        .await
        .expect("fence old capability");
    let binding = lab
        .lookup_fixture(&fenced, &FixtureFrame::Admission(&admission))
        .await
        .expect("locked lookup")
        .expect("committed slot");
    assert_eq!((binding.ordinal, binding.h), (1, 1));
    let stanza = other("next");
    let next = FixtureFrame::Other {
        id: uuid::Uuid::new_v4(),
        stanza: &stanza,
    };
    assert!(matches!(
        lab.allocate_fixture(fenced.capability(), &next).await,
        Err(FixtureAllocationError::Unresolved)
    ));
    lab.reconcile_fixture_slot(fenced.capability(), &binding)
        .await
        .expect("synthetic resolution");

    // Hold the stream lock while an allocation is submitted, then cancel its
    // future. Fencing serializes against any database work that survived it.
    let app = format!("allocation_cancel_{}", uuid::Uuid::new_v4().simple());
    let cancelling_lab = Arc::new(
        DirectAdmissionLab::postgres(
            fixture.runtime_options.clone().application_name(&app),
            config,
            1,
            typed_bounds(),
            admission.connection.clone(),
            limits,
        )
        .expect("cancelling lab"),
    );
    let mut holder = fixture.admin.begin().await.expect("holder");
    sqlx::query(&format!(
        "SELECT 1 FROM {}.direct_fixture_stream WHERE stream_id = $1 FOR UPDATE",
        fixture.target
    ))
    .bind(stream.id)
    .fetch_one(&mut *holder)
    .await
    .expect("lock stream");
    let pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *holder)
        .await
        .expect("pid");
    let cancel_id = uuid::Uuid::new_v4();
    let task = {
        let lab = cancelling_lab.clone();
        let cap = fenced.capability().clone();
        let stanza = stanza.clone();
        tokio::spawn(async move {
            lab.allocate_fixture(
                &cap,
                &FixtureFrame::Other {
                    id: cancel_id,
                    stanza: &stanza,
                },
            )
            .await
        })
    };
    wait_for_blocked_application(&fixture.admin, &app, pid).await;
    task.abort();
    assert!(task.await.expect_err("cancelled").is_cancelled());
    holder.commit().await.expect("release");
    let fenced_again = lab
        .fence_fixture_capability(fenced.capability(), uuid::Uuid::new_v4())
        .await
        .expect("fence cancelled capability");
    let cancelled_frame = FixtureFrame::Other {
        id: cancel_id,
        stanza: &stanza,
    };
    let observed = lab
        .lookup_fixture(&fenced_again, &cancelled_frame)
        .await
        .expect("post-fence lookup");
    assert!(matches!(
        cancelling_lab
            .allocate_fixture(fenced.capability(), &cancelled_frame)
            .await,
        Err(FixtureAllocationError::Rejected)
    ));
    if let Some(binding) = observed {
        lab.reconcile_fixture_slot(fenced_again.capability(), &binding)
            .await
            .expect("reconcile observed slot");
    }
    let final_binding = lab
        .allocate_fixture(fenced_again.capability(), &next)
        .await
        .expect("next safe slot");
    assert!(final_binding.ordinal == 2 || final_binding.ordinal == 3);
    drop(cancelling_lab);
    drop(lab);
    fixture.cleanup().await;
}

#[tokio::test]
async fn sqlite_fixture_fence_tokens_never_revalidate_and_are_bounded() {
    let (db, _) = sqlite_fixture().await;
    install_sqlite(&db).await;
    let connection =
        request(direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop").0).connection;
    let lab = DirectAdmissionLab::sqlite(
        &db,
        typed_bounds(),
        connection.clone(),
        DirectAdmissionLimits::new(2, 4096).expect("limits"),
    )
    .expect("lab");
    let stream = stream(&connection);
    let initial_token = uuid::Uuid::new_v4();
    let initial = lab
        .open_fixture_stream(&stream, initial_token)
        .await
        .expect("stream");
    let next_token = uuid::Uuid::new_v4();
    let first_fence = lab
        .fence_fixture_capability(&initial, next_token)
        .await
        .expect("first fence");
    let second_fence = lab
        .fence_fixture_capability(first_fence.capability(), uuid::Uuid::new_v4())
        .await
        .expect("second fence");
    assert!(matches!(
        lab.fence_fixture_capability(second_fence.capability(), initial_token)
            .await,
        Err(FixtureAllocationError::Rejected)
    ));
    assert!(matches!(
        lab.fence_fixture_capability(second_fence.capability(), next_token)
            .await,
        Err(FixtureAllocationError::Rejected)
    ));
    let stanza = other("stale");
    let frame = FixtureFrame::Other {
        id: uuid::Uuid::new_v4(),
        stanza: &stanza,
    };
    assert!(matches!(
        lab.allocate_fixture(&initial, &frame).await,
        Err(FixtureAllocationError::Rejected)
    ));
    assert!(matches!(
        lab.lookup_fixture(&first_fence, &frame).await,
        Err(FixtureAllocationError::Rejected)
    ));
    let mut current = second_fence.capability().clone();
    // Initial capability plus 63 distinct rotations exhausts the per-stream cap.
    for _ in 0..61 {
        current = lab
            .fence_fixture_capability(&current, uuid::Uuid::new_v4())
            .await
            .expect("bounded rotation")
            .capability()
            .clone();
    }
    assert_eq!(
        sqlite_count(&db, "SELECT count(*) FROM main.direct_fixture_capability").await,
        64
    );
    assert!(matches!(
        lab.fence_fixture_capability(&current, uuid::Uuid::new_v4())
            .await,
        Err(FixtureAllocationError::Capacity)
    ));
    lab.retire_fixture_stream(&current)
        .await
        .expect("exhausted stream can retire");
}

#[tokio::test]
async fn sqlite_fixture_counts_message_presence_and_iq_together() {
    let (db, _) = sqlite_fixture().await;
    install_sqlite(&db).await;
    let connection =
        request(direct_pin(uuid::Uuid::new_v4(), "bob@example.test/laptop").0).connection;
    let lab = DirectAdmissionLab::sqlite(
        &db,
        typed_bounds(),
        connection.clone(),
        DirectAdmissionLimits::new(3, 4096).expect("limits"),
    )
    .expect("lab");
    let stream = stream(&connection);
    let cap = lab
        .open_fixture_stream(&stream, uuid::Uuid::new_v4())
        .await
        .expect("stream");
    let stanzas = [
        other("message"),
        Stanza::Presence(xmpp_parsers::presence::Presence::available()),
        Stanza::Iq(Box::new(xmpp_parsers::iq::Iq::Result {
            from: None,
            to: None,
            id: "fixture".to_owned(),
            payload: None,
        })),
    ];
    for (index, stanza) in stanzas.iter().enumerate() {
        let frame = FixtureFrame::Other {
            id: uuid::Uuid::new_v4(),
            stanza,
        };
        let binding = lab
            .allocate_fixture(&cap, &frame)
            .await
            .expect("countable frame");
        assert_eq!(binding.ordinal, u64::try_from(index).expect("index") + 1);
        assert_eq!(
            lab.allocate_fixture(&cap, &frame)
                .await
                .expect("exact replay"),
            binding
        );
        lab.reconcile_fixture_slot(&cap, &binding)
            .await
            .expect("synthetic reconciliation");
    }
    assert_eq!(
        sqlite_count(&db, "SELECT count(*) FROM main.direct_fixture_slot").await,
        3
    );
}
