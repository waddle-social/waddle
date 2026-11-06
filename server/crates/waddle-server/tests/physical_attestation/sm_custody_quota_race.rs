//! Raw-writer snapshot races for the unselected PG17 custody schema.

use sqlx::{Connection, PgConnection};
use uuid::Uuid;

use super::super::super::super::wait_for_blocked_application;
use super::{pg_seed_transaction, Candidate, CATALOG_GRANT_LOCK};

async fn parent(c: &Candidate) -> Uuid {
    let key = Uuid::new_v4();
    let mut tx = pg_seed_transaction(&c.fixture).await;
    sqlx::query(&format!("INSERT INTO {}.ingress_messages (message_key,digest_version,digest,terminal_at,retention_eligible_at) VALUES ($1,1,$2,clock_timestamp(),NULL)",c.fixture.target))
        .bind(key).bind([7_u8; 32].as_slice()).execute(&mut *tx).await.expect("bare canonical parent");
    tx.commit().await.expect("parent commit");
    key
}

async fn begin(conn: &mut PgConnection, isolation: &str) {
    sqlx::query(&format!("BEGIN ISOLATION LEVEL {isolation}"))
        .execute(conn)
        .await
        .expect("begin raw transaction");
}

fn raw_insert(q: &str) -> String {
    format!("INSERT INTO {q}.sm_custody_operations (operation_id,gate_id,message_key,receipt_kind,semantic_identity_hash,resource,attempt_token,payload,received_at) VALUES ($1,$2,$3,1,$4,$5,$6,$7,clock_timestamp())")
}

async fn insert_raw(
    conn: &mut PgConnection,
    q: &str,
    gate: Uuid,
    key: Uuid,
    payload: &str,
) -> Result<(), sqlx::Error> {
    sqlx::query(&raw_insert(q))
        .bind(Uuid::new_v4())
        .bind(gate)
        .bind(key)
        .bind([1_u8; 32].as_slice())
        .bind("bob@example.test/laptop")
        .bind(Uuid::new_v4())
        .bind(payload)
        .execute(conn)
        .await
        .map(|_| ())
}

async fn run_quota_race(isolation: &str, byte_limit: bool) {
    let c = Candidate::new().await;
    let q = &c.fixture.target;
    let seed_parent = parent(&c).await;
    let left_parent = parent(&c).await;
    let right_parent = parent(&c).await;
    let other_gate = Uuid::new_v4();
    sqlx::query(&format!("INSERT INTO {q}.sm_custody_gates (gate_id,token,resource,session_id,generation,endpoint,node_id,node_incarnation,claim_epoch) SELECT $1,$2,resource,'quota-race-other',generation,endpoint,node_id,node_incarnation,claim_epoch FROM {q}.sm_custody_gates WHERE gate_id=$3"))
        .bind(other_gate).bind(Uuid::new_v4()).bind(c.capture.gate_id).execute(&c.fixture.admin).await.expect("independent gate");
    let (seeds, payload) = if byte_limit {
        (63_i32, "é".repeat(131072))
    } else {
        (1023_i32, "x".to_owned())
    };
    sqlx::query(&format!("INSERT INTO {q}.sm_custody_operations (operation_id,gate_id,message_key,receipt_kind,semantic_identity_hash,resource,attempt_token,payload,received_at) SELECT gen_random_uuid(),$1,$2,1,decode(lpad(to_hex(n),64,'0'),'hex'),'bob@example.test/laptop',gen_random_uuid(),$3,clock_timestamp() FROM generate_series(1,$4) AS n"))
        .bind(c.capture.gate_id).bind(seed_parent).bind(&payload).bind(seeds)
        .execute(&c.fixture.admin).await.expect("seed just below hard quota");
    let expected_bytes = if byte_limit {
        16_i64 * 1024 * 1024
    } else {
        1024
    };
    let mut left = PgConnection::connect_with(&c.fixture.runtime_options)
        .await
        .expect("left raw runtime");
    let mut right = PgConnection::connect_with(
        &c.fixture
            .runtime_options
            .clone()
            .application_name("sm-raw-quota-waiter"),
    )
    .await
    .expect("right raw runtime");
    let left_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut left)
        .await
        .expect("left pid");
    begin(&mut left, isolation).await;
    begin(&mut right, isolation).await;
    let before: (i64, i64) = sqlx::query_as(&format!("SELECT count(*),coalesce(sum(octet_length(payload)),0)::bigint FROM {q}.sm_custody_operations"))
        .fetch_one(&mut right).await.expect("pin contender snapshot");
    assert_eq!(before.0, i64::from(seeds));
    assert_eq!(before.1, expected_bytes - payload.len() as i64);
    insert_raw(&mut left, q, c.capture.gate_id, left_parent, &payload)
        .await
        .expect("winner inserts under held quota row");
    let query = raw_insert(q);
    let contender_payload = payload.clone();
    let contender = tokio::spawn(async move {
        let result = sqlx::query(&query)
            .bind(Uuid::new_v4())
            .bind(other_gate)
            .bind(right_parent)
            .bind([1_u8; 32].as_slice())
            .bind("bob@example.test/laptop")
            .bind(Uuid::new_v4())
            .bind(contender_payload)
            .execute(&mut right)
            .await;
        if result.is_ok() {
            sqlx::query("COMMIT")
                .execute(&mut right)
                .await
                .expect("unexpected contender commit");
        } else {
            sqlx::query("ROLLBACK")
                .execute(&mut right)
                .await
                .expect("contender rollback");
        }
        result
    });
    wait_for_blocked_application(&c.fixture.admin, "sm-raw-quota-waiter", left_pid).await;
    sqlx::query("COMMIT")
        .execute(&mut left)
        .await
        .expect("winner commit");
    let error = contender
        .await
        .expect("contender task")
        .expect_err("stale or over-quota contender must fail");
    let db = error.as_database_error().expect("database error");
    if isolation == "REPEATABLE READ" {
        assert_eq!(
            db.code().as_deref(),
            Some("40001"),
            "stale quota row update must serialize-fail"
        );
    } else {
        assert_eq!(db.code().as_deref(), Some("P0001"));
        assert!(db.message().contains("quota exceeded"));
    }
    let after: (i64, i64, i64) = sqlx::query_as(&format!("SELECT (SELECT count(*) FROM {q}.sm_custody_operations),(SELECT sum(octet_length(payload))::bigint FROM {q}.sm_custody_operations),(SELECT revision FROM {q}.sm_custody_quota WHERE id=1)"))
        .fetch_one(&c.fixture.admin).await.expect("committed quota evidence");
    assert_eq!(
        after,
        (i64::from(seeds) + 1, expected_bytes, i64::from(seeds) + 1)
    );
    drop(left);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_raw_custody_quota_fences_stale_snapshots_at_row_and_byte_limits() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    for isolation in ["REPEATABLE READ", "READ COMMITTED"] {
        for bytes in [false, true] {
            run_quota_race(isolation, bytes).await;
        }
    }
}

async fn run_gc_race(isolation: &str) {
    let c = Candidate::new().await;
    let q = &c.fixture.target;
    let key = parent(&c).await;
    // The exact parent has no unrelated children: a deletion succeeds before
    // admission and is rolled back, so another FK cannot mask this fence.
    let mut baseline = c.fixture.admin.begin().await.expect("baseline delete");
    assert_eq!(
        sqlx::query(&format!(
            "DELETE FROM {q}.ingress_messages WHERE message_key=$1"
        ))
        .bind(key)
        .execute(&mut *baseline)
        .await
        .expect("unreferenced parent delete")
        .rows_affected(),
        1
    );
    baseline.rollback().await.expect("restore parent");
    let mut left = PgConnection::connect_with(&c.fixture.runtime_options)
        .await
        .expect("raw admission");
    let mut right = PgConnection::connect_with(
        &c.fixture
            .admin_options
            .clone()
            .application_name("sm-raw-gc-waiter"),
    )
    .await
    .expect("raw collector");
    let left_pid: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut left)
        .await
        .expect("left pid");
    begin(&mut right, isolation).await;
    let exists: bool = sqlx::query_scalar(&format!(
        "SELECT EXISTS (SELECT 1 FROM {q}.ingress_messages WHERE message_key=$1)"
    ))
    .bind(key)
    .fetch_one(&mut right)
    .await
    .expect("pin collector snapshot");
    assert!(exists);
    begin(&mut left, "READ COMMITTED").await;
    insert_raw(&mut left, q, c.capture.gate_id, key, "x")
        .await
        .expect("raw pending admission");
    let delete = format!("DELETE FROM {q}.ingress_messages WHERE message_key=$1");
    let collector = tokio::spawn(async move {
        let result = sqlx::query(&delete).bind(key).execute(&mut right).await;
        if result.is_ok() {
            sqlx::query("COMMIT")
                .execute(&mut right)
                .await
                .expect("unexpected collector commit");
        } else {
            sqlx::query("ROLLBACK")
                .execute(&mut right)
                .await
                .expect("collector rollback");
        }
        result
    });
    wait_for_blocked_application(&c.fixture.admin, "sm-raw-gc-waiter", left_pid).await;
    sqlx::query("COMMIT")
        .execute(&mut left)
        .await
        .expect("admission commit");
    let error = collector
        .await
        .expect("collector task")
        .expect_err("pending custody must fence collection");
    let db = error.as_database_error().expect("database error");
    if isolation == "REPEATABLE READ" {
        assert_eq!(
            db.code().as_deref(),
            Some("40001"),
            "stale parent delete must serialize-fail"
        );
    } else {
        assert_eq!(db.code().as_deref(), Some("P0001"));
        assert!(db
            .message()
            .contains("pending SM custody prevents canonical deletion"));
    }
    let after: (bool, i64) = sqlx::query_as(&format!("SELECT EXISTS (SELECT 1 FROM {q}.ingress_messages WHERE message_key=$1),(SELECT count(*) FROM {q}.sm_custody_operations WHERE message_key=$1)"))
        .bind(key).fetch_one(&c.fixture.admin).await.expect("retained parent and custody");
    assert_eq!(after, (true, 1));
    drop(left);
    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_raw_custody_admission_fences_stale_collector_snapshot() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    for isolation in ["REPEATABLE READ", "READ COMMITTED"] {
        run_gc_race(isolation).await;
    }
}
