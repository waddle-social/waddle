//! Disposable, unregistered P3 relational contract laboratory.
//!
//! The synthetic tables, direct SQL and cooperating writers do not prove
//! production claim authorization, P2 custody, endpoint publication or rollout.

use sqlx::{Connection, PgConnection, Row};
use tokio::sync::oneshot;
use uuid::Uuid;

const CANDIDATE: &str = include_str!("p3_joint_identity/schema.sql");
const SLOT: &str = "00000000-0000-0000-0000-000000000001";
const RESOURCE: &str = "00000000-0000-0000-0000-000000000002";
const OCCUPANCY: &str = "00000000-0000-0000-0000-000000000003";
const ACTOR: &str = "00000000-0000-0000-0000-000000000004";
const BINDING: &str = "00000000-0000-0000-0000-000000000005";
const PHYSICAL: &str = "00000000-0000-0000-0000-000000000006";
const SESSION: &str = "00000000-0000-0000-0000-000000000007";
const AUTH: &str = "00000000-0000-0000-0000-000000000008";
const NEW_BINDING: &str = "00000000-0000-0000-0000-000000000009";
const NEW_PHYSICAL: &str = "00000000-0000-0000-0000-00000000000a";
const NEW_SESSION: &str = "00000000-0000-0000-0000-00000000000b";

struct Fixture {
    url: String,
    schema: String,
}

impl Fixture {
    async fn new() -> Option<Self> {
        let url = waddle_ws_test_support::require_test_postgres_url()?;
        let mut admin = PgConnection::connect(&url)
            .await
            .expect("connect test postgres");
        let version: String = sqlx::query_scalar("SHOW server_version_num")
            .fetch_one(&mut admin)
            .await
            .expect("postgres version");
        let version: u32 = version.parse().expect("numeric server version");
        assert!(
            (170000..180000).contains(&version),
            "expected PostgreSQL 17, got {version}"
        );
        let schema = format!("p3_identity_{}", Uuid::new_v4().simple());
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&mut admin)
            .await
            .expect("create isolated schema");
        sqlx::query(&format!("SET search_path TO {schema}"))
            .execute(&mut admin)
            .await
            .expect("select isolated schema");
        sqlx::raw_sql(CANDIDATE)
            .execute(&mut admin)
            .await
            .expect("install candidate");
        Some(Self { url, schema })
    }

    async fn connect(&self) -> PgConnection {
        let mut conn = PgConnection::connect(&self.url)
            .await
            .expect("connect fixture");
        sqlx::query(&format!("SET search_path TO {}", self.schema))
            .execute(&mut conn)
            .await
            .expect("select fixture schema");
        conn
    }

    async fn cleanup(self) {
        let mut conn = PgConnection::connect(&self.url)
            .await
            .expect("connect cleanup");
        sqlx::query(&format!("DROP SCHEMA {} CASCADE", self.schema))
            .execute(&mut conn)
            .await
            .expect("drop own test schema");
    }
}

async fn seed(conn: &mut PgConnection) {
    let mut tx = conn.begin().await.expect("begin seed");
    sqlx::query("INSERT INTO p3_user_claim VALUES ('alice@example.test', 'account-node', 'lease-a', 4, $1::uuid)")
        .bind(ACTOR).execute(&mut *tx).await.expect("claim fixture");
    sqlx::query("INSERT INTO p3_slot VALUES ($1::uuid, 'alice@example.test', 'alice@example.test/phone', 1, 1, $2::uuid, 1)")
        .bind(SLOT).bind(RESOURCE).execute(&mut *tx).await.expect("slot fixture");
    sqlx::query("INSERT INTO p3_resource VALUES ($1::uuid, $2::uuid, 'alice@example.test', $3::uuid, 'account-node', 'lease-a', 4, $4::uuid, $5::uuid, 1, 'active', $6::uuid, 1)")
        .bind(RESOURCE).bind(SLOT).bind(OCCUPANCY).bind(ACTOR).bind(BINDING).bind(SESSION)
        .execute(&mut *tx).await.expect("resource fixture");
    sqlx::query(
        "INSERT INTO p3_binding VALUES ($1::uuid, $2::uuid, 'socket-node', 'lease-b', $3::uuid, 1)",
    )
    .bind(BINDING)
    .bind(RESOURCE)
    .bind(PHYSICAL)
    .execute(&mut *tx)
    .await
    .expect("binding fixture");
    sqlx::query(
        "INSERT INTO p3_sm_identity VALUES ($1::uuid, $2, 'alice@example.test', $3::uuid, 3, 7)",
    )
    .bind(SESSION)
    .bind(vec![42u8; 32])
    .bind(AUTH)
    .execute(&mut *tx)
    .await
    .expect("stable sm identity fixture");
    sqlx::query("INSERT INTO p3_sm_session VALUES ($1::uuid, $2::uuid, 'alice@example.test', $3::uuid, 3, 7, 'sm-node', 'lease-c', 5, 1, $4::uuid, 1, 'active', 'live', 0, 0, 0, 0)")
        .bind(SESSION).bind(RESOURCE).bind(AUTH).bind(BINDING)
        .execute(&mut *tx).await.expect("sm fixture");
    tx.commit()
        .await
        .expect("deferred exact references commit together");
}

#[tokio::test]
async fn exact_optional_sm_and_claim_constraints_are_deferred_and_fail_closed() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let mut conn = fixture.connect().await;
    seed(&mut conn).await;

    let result =
        sqlx::query("UPDATE p3_resource SET sm_generation = NULL WHERE resource_id = $1::uuid")
            .bind(RESOURCE)
            .execute(&mut conn)
            .await;
    assert!(
        result.is_err(),
        "partial optional SM reference must fail immediately"
    );

    let mut tx = conn.begin().await.unwrap();
    sqlx::query("UPDATE p3_resource SET sm_generation = 2 WHERE resource_id = $1::uuid")
        .bind(RESOURCE)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(
        tx.commit().await.is_err(),
        "wrong exact SM generation must fail at commit"
    );

    let mut tx = conn.begin().await.unwrap();
    sqlx::query("UPDATE p3_resource SET actor_incarnation = $1::uuid WHERE resource_id = $2::uuid")
        .bind(Uuid::new_v4().to_string())
        .bind(RESOURCE)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(
        tx.commit().await.is_err(),
        "wrong account actor incarnation must fail at commit"
    );

    let mut tx = conn.begin().await.unwrap();
    sqlx::query("UPDATE p3_sm_session SET principal_bare_jid = 'mallory@example.test' WHERE sm_key = $1::uuid")
        .bind(SESSION).execute(&mut *tx).await.unwrap();
    assert!(
        tx.commit().await.is_err(),
        "principal must belong to resource account"
    );

    let mut tx = conn.begin().await.unwrap();
    sqlx::query(
        "UPDATE p3_resource SET sm_key = NULL, sm_generation = NULL WHERE resource_id = $1::uuid",
    )
    .bind(RESOURCE)
    .execute(&mut *tx)
    .await
    .unwrap();
    assert!(
        tx.commit().await.is_err(),
        "an SM row cannot outlive its exact reciprocal link"
    );
    drop(conn);
    fixture.cleanup().await;
}

#[tokio::test]
async fn detach_and_resume_switch_the_exact_binding_as_one_transaction() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let mut conn = fixture.connect().await;
    seed(&mut conn).await;
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("UPDATE p3_resource SET state = 'detached' WHERE resource_id = $1::uuid")
        .bind(RESOURCE)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE p3_sm_session SET state = 'detached', resource_state = 'detached' WHERE sm_key = $1::uuid")
        .bind(SESSION).execute(&mut *tx).await.unwrap();
    tx.commit().await.expect("detach both sides together");
    let active: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM p3_resource WHERE resource_id = $1::uuid AND state = 'active'",
    )
    .bind(RESOURCE)
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(
        active, 0,
        "detached resource is not selected by this fixture"
    );

    let mut tx = conn.begin().await.unwrap();
    sqlx::query("UPDATE p3_resource SET state = 'active' WHERE resource_id = $1::uuid")
        .bind(RESOURCE)
        .execute(&mut *tx)
        .await
        .unwrap();
    assert!(
        tx.commit().await.is_err(),
        "resource and SM lifecycle cannot diverge"
    );

    let mut tx = conn.begin().await.unwrap();
    sqlx::query("INSERT INTO p3_binding VALUES ($1::uuid, $2::uuid, 'socket-node-2', 'lease-d', $3::uuid, 2)")
        .bind(NEW_BINDING).bind(RESOURCE).bind(NEW_PHYSICAL).execute(&mut *tx).await.unwrap();
    let old = sqlx::query("UPDATE p3_slot SET revision = revision + 1, install_counter = 2, current_install = 2 WHERE slot_id = $1::uuid AND revision = 1 AND current_resource = $2::uuid AND current_install = 1")
        .bind(SLOT).bind(RESOURCE).execute(&mut *tx).await.unwrap();
    assert_eq!(old.rows_affected(), 1);
    let res = sqlx::query("UPDATE p3_resource SET binding_id = $1::uuid, install_generation = 2, sm_generation = 2, state = 'prepared' WHERE resource_id = $2::uuid AND binding_id = $3::uuid AND install_generation = 1 AND sm_generation = 1 AND state = 'detached' AND actor_incarnation = $4::uuid")
        .bind(NEW_BINDING).bind(RESOURCE).bind(BINDING).bind(ACTOR).execute(&mut *tx).await.unwrap();
    assert_eq!(res.rows_affected(), 1);
    let sm = sqlx::query("UPDATE p3_sm_session SET binding_id = $1::uuid, install_generation = 2, sm_generation = 2, resource_state = 'prepared', state = 'transfer_hydrating' WHERE sm_key = $2::uuid AND binding_id = $3::uuid AND sm_generation = 1 AND state = 'detached' AND auth_context_id = $4::uuid AND auth_context_version = 3 AND principal_auth_epoch = 7 AND sm_claim_epoch = 5")
        .bind(NEW_BINDING).bind(SESSION).bind(BINDING).bind(AUTH).execute(&mut *tx).await.unwrap();
    assert_eq!(sm.rows_affected(), 1);
    tx.commit()
        .await
        .expect("deferred refs follow the new fence atomically");

    let row = sqlx::query("SELECT resource_id::text, occupancy_id::text, binding_id::text, install_generation, sm_generation, state FROM p3_resource WHERE resource_id = $1::uuid")
        .bind(RESOURCE).fetch_one(&mut conn).await.unwrap();
    assert_eq!(row.get::<String, _>(0), RESOURCE);
    assert_eq!(row.get::<String, _>(1), OCCUPANCY);
    assert_eq!(row.get::<String, _>(2), NEW_BINDING);
    assert_eq!((row.get::<i64, _>(3), row.get::<i64, _>(4)), (2, 2));
    assert_eq!(
        row.get::<String, _>(5),
        "prepared",
        "not active before publication"
    );
    let stale = sqlx::query("UPDATE p3_resource SET state = 'retiring' WHERE resource_id = $1::uuid AND binding_id = $2::uuid AND install_generation = 1")
        .bind(RESOURCE).bind(BINDING).execute(&mut conn).await.unwrap();
    assert_eq!(
        stale.rows_affected(),
        0,
        "old cleanup cannot mutate successor"
    );
    drop(conn);
    fixture.cleanup().await;
}

#[tokio::test]
async fn terminal_archive_retains_old_payload_while_a_new_session_is_enabled() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let mut conn = fixture.connect().await;
    seed(&mut conn).await;
    sqlx::query("INSERT INTO p3_sm_retained VALUES ($1::uuid, 1, $2)")
        .bind(SESSION)
        .bind(b"old-opaque-payload".to_vec())
        .execute(&mut conn)
        .await
        .unwrap();
    sqlx::query("UPDATE p3_sm_session SET exposed = 1, appended = 1 WHERE sm_key = $1::uuid")
        .bind(SESSION)
        .execute(&mut conn)
        .await
        .unwrap();
    let mut tx = conn.begin().await.unwrap();
    sqlx::query("UPDATE p3_resource SET state = 'retiring' WHERE resource_id = $1::uuid")
        .bind(RESOURCE)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query("UPDATE p3_sm_session SET state = 'terminal', resource_state = 'retiring' WHERE sm_key = $1::uuid")
        .bind(SESSION).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO p3_sm_tombstone SELECT s.sm_key, s.resource_id, r.occupancy_id, s.binding_id, s.install_generation, s.sm_generation, s.sm_owner_node, s.sm_owner_epoch, s.sm_claim_epoch, r.user_owner_node, r.user_owner_epoch, r.user_claim_epoch, r.actor_incarnation, s.peer_acked, s.exposed, s.appended, s.inbound_handled FROM p3_sm_session s JOIN p3_resource r USING (resource_id) WHERE s.sm_key = $1::uuid AND s.state = 'terminal'")
        .bind(SESSION).execute(&mut *tx).await.unwrap();
    sqlx::query("DELETE FROM p3_sm_session WHERE sm_key = $1::uuid AND state = 'terminal'")
        .bind(SESSION)
        .execute(&mut *tx)
        .await
        .unwrap();
    sqlx::query(
        "INSERT INTO p3_sm_identity VALUES ($1::uuid, $2, 'alice@example.test', $3::uuid, 3, 7)",
    )
    .bind(NEW_SESSION)
    .bind(vec![43u8; 32])
    .bind(AUTH)
    .execute(&mut *tx)
    .await
    .unwrap();
    sqlx::query("UPDATE p3_resource SET state = 'active', sm_key = $1::uuid, sm_generation = 1 WHERE resource_id = $2::uuid")
        .bind(NEW_SESSION).bind(RESOURCE).execute(&mut *tx).await.unwrap();
    sqlx::query("INSERT INTO p3_sm_session VALUES ($1::uuid, $2::uuid, 'alice@example.test', $3::uuid, 3, 7, 'sm-node', 'lease-c', 6, 1, $4::uuid, 1, 'active', 'enabling_live', 0, 0, 0, 0)")
        .bind(NEW_SESSION).bind(RESOURCE).bind(AUTH).bind(BINDING).execute(&mut *tx).await.unwrap();
    tx.commit()
        .await
        .expect("archive and replacement commit atomically");
    let retained: Vec<u8> = sqlx::query_scalar(
        "SELECT payload FROM p3_sm_retained WHERE sm_key = $1::uuid AND ordinal = 1",
    )
    .bind(SESSION)
    .fetch_one(&mut conn)
    .await
    .unwrap();
    assert_eq!(retained, b"old-opaque-payload");
    let old_final: i64 =
        sqlx::query_scalar("SELECT appended FROM p3_sm_tombstone WHERE sm_key = $1::uuid")
            .bind(SESSION)
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert_eq!(old_final, 1);
    let current: String =
        sqlx::query_scalar("SELECT sm_key::text FROM p3_resource WHERE resource_id = $1::uuid")
            .bind(RESOURCE)
            .fetch_one(&mut conn)
            .await
            .unwrap();
    assert_eq!(current, NEW_SESSION);
    drop(conn);
    fixture.cleanup().await;
}

#[tokio::test]
async fn operation_barrier_fences_a_delayed_writer_and_preserves_committed_outcomes() {
    let Some(fixture) = Fixture::new().await else {
        return;
    };
    let mut control = fixture.connect().await;
    let operation = Uuid::new_v4();
    let digest = vec![71u8; 32];
    sqlx::query("INSERT INTO p3_operation VALUES ($1, $2, 'pending', NULL)")
        .bind(operation)
        .bind(&digest)
        .execute(&mut control)
        .await
        .unwrap();
    let (go, wait) = oneshot::channel();
    let mut writer = fixture.connect().await;
    let expected = digest.clone();
    let delayed = tokio::spawn(async move {
        wait.await.unwrap();
        let mut tx = writer.begin().await.unwrap();
        let row = sqlx::query(
            "SELECT payload_sha256, state FROM p3_operation WHERE operation_id = $1 FOR UPDATE",
        )
        .bind(operation)
        .fetch_one(&mut *tx)
        .await
        .unwrap();
        let payload: Vec<u8> = row.get(0);
        let state: String = row.get(1);
        let can_commit = payload == expected && state == "pending";
        if can_commit {
            sqlx::query("UPDATE p3_operation SET state = 'committed', result_revision = 9 WHERE operation_id = $1")
                .bind(operation).execute(&mut *tx).await.unwrap();
        }
        tx.commit().await.unwrap();
        can_commit
    });
    let mut barrier = control.begin().await.unwrap();
    let state: String = sqlx::query_scalar(
        "SELECT state FROM p3_operation WHERE operation_id = $1 AND payload_sha256 = $2 FOR UPDATE",
    )
    .bind(operation)
    .bind(&digest)
    .fetch_one(&mut *barrier)
    .await
    .unwrap();
    assert_eq!(state, "pending");
    sqlx::query("UPDATE p3_operation SET state = 'canceled' WHERE operation_id = $1")
        .bind(operation)
        .execute(&mut *barrier)
        .await
        .unwrap();
    barrier.commit().await.unwrap();
    go.send(()).unwrap();
    assert!(
        !delayed.await.unwrap(),
        "delayed writer must recheck fenced row"
    );
    let state: String =
        sqlx::query_scalar("SELECT state FROM p3_operation WHERE operation_id = $1")
            .bind(operation)
            .fetch_one(&mut control)
            .await
            .unwrap();
    assert_eq!(state, "canceled");
    let mismatch: Option<String> = sqlx::query_scalar(
        "SELECT state FROM p3_operation WHERE operation_id = $1 AND payload_sha256 = $2",
    )
    .bind(operation)
    .bind(vec![72u8; 32])
    .fetch_optional(&mut control)
    .await
    .unwrap();
    assert_eq!(
        mismatch, None,
        "same operation ID with another payload cannot be reconciled as a match"
    );

    let committed = Uuid::new_v4();
    sqlx::query("INSERT INTO p3_operation VALUES ($1, $2, 'pending', NULL)")
        .bind(committed)
        .bind(&digest)
        .execute(&mut control)
        .await
        .unwrap();
    let mut writer = fixture.connect().await;
    let mut tx = writer.begin().await.unwrap();
    let _: String =
        sqlx::query_scalar("SELECT state FROM p3_operation WHERE operation_id = $1 FOR UPDATE")
            .bind(committed)
            .fetch_one(&mut *tx)
            .await
            .unwrap();
    sqlx::query(
        "UPDATE p3_operation SET state = 'committed', result_revision = 42 WHERE operation_id = $1",
    )
    .bind(committed)
    .execute(&mut *tx)
    .await
    .unwrap();
    let mut reconciler = fixture.connect().await;
    let reconciler_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut reconciler)
        .await
        .unwrap();
    let expected_digest = digest.clone();
    let waiting = tokio::spawn(async move {
        let mut reconciliation = reconciler.begin().await.unwrap();
        let row = sqlx::query("SELECT state, result_revision FROM p3_operation WHERE operation_id = $1 AND payload_sha256 = $2 FOR UPDATE")
            .bind(committed).bind(&expected_digest).fetch_one(&mut *reconciliation).await.unwrap();
        let result = (row.get::<String, _>(0), row.get::<i64, _>(1));
        reconciliation.commit().await.unwrap();
        result
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let blocked: bool = sqlx::query_scalar("SELECT cardinality(pg_blocking_pids($1)) > 0")
                .bind(reconciler_pid)
                .fetch_one(&mut control)
                .await
                .unwrap();
            if blocked {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("reconciler waits for original transaction");
    tx.commit().await.unwrap(); // model a lost response after commit
    assert_eq!(waiting.await.unwrap(), ("committed".to_owned(), 42));
    drop(writer);
    drop(control);
    fixture.cleanup().await;
}
