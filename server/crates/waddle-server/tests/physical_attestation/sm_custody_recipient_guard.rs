//! PostgreSQL 17 races for the unregistered recipient-wide route guard.

use std::time::Duration;

use waddle_server::sm_custody::CustodyError;

use super::multiframe::source;
use super::{Candidate, CATALOG_GRANT_LOCK};

#[tokio::test]
async fn postgres_recipient_transition_waits_for_delayed_candidate_writer() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let (frame, lease) = source(&c, "delayed recipient candidate").await;
    let recipient = c.capture.resource.to_bare().to_string();

    let mut transition = c.fixture.admin.begin().await.expect("transition tx");
    sqlx::query(&format!(
        "INSERT INTO {}.sm_custody_recipient_route_guards (recipient) VALUES ($1) ON CONFLICT DO NOTHING",
        c.fixture.target
    ))
    .bind(&recipient)
    .execute(&mut *transition)
    .await
    .expect("seed recipient route row");
    sqlx::query(&format!(
        "SELECT state FROM {}.sm_custody_recipient_route_guards WHERE recipient = $1 FOR UPDATE",
        c.fixture.target
    ))
    .bind(&recipient)
    .fetch_one(&mut *transition)
    .await
    .expect("hold route row");
    let blocker: i32 = sqlx::query_scalar("SELECT pg_backend_pid()")
        .fetch_one(&mut *transition)
        .await
        .expect("blocker pid");

    let admission = tokio::spawn(async move { repo.admit(&frame, &lease).await });
    wait_for_blocker(&c.fixture.admin, blocker).await;

    sqlx::query(&format!(
        "UPDATE {}.sm_custody_recipient_route_guards SET state = 1, revision = revision + 1 WHERE recipient = $1",
        c.fixture.target
    ))
    .bind(&recipient)
    .execute(&mut *transition)
    .await
    .expect("close candidate route");
    transition.commit().await.expect("commit route transition");
    assert!(matches!(
        admission.await.expect("admission task"),
        Err(CustodyError::Rejected)
    ));

    let count: i64 = sqlx::query_scalar(&format!(
        "SELECT count(*) FROM {}.sm_custody_operations",
        c.fixture.target
    ))
    .fetch_one(&c.fixture.admin)
    .await
    .expect("candidate journal count");
    assert_eq!(count, 0, "the delayed writer did not enter custody");

    let mut reopen = c.fixture.admin.begin().await.expect("reopen tx");
    assert!(sqlx::query(&format!(
        "UPDATE {}.sm_custody_recipient_route_guards SET state = 0, revision = revision + 1 WHERE recipient = $1",
        c.fixture.target
    ))
    .bind(&recipient)
    .execute(&mut *reopen)
    .await
    .is_err());
    reopen.rollback().await.expect("rollback rejected reopen");

    c.fixture.cleanup().await;
}

#[tokio::test]
async fn postgres_same_recipient_sources_serialize_without_deadlock_and_rollback_stays_open() {
    let _serial = CATALOG_GRANT_LOCK.lock().await;
    let c = Candidate::new().await;
    let repo = c.repository();
    let first = source(&c, "source one").await;
    let second = source(&c, "source two").await;
    let recipient = c.capture.resource.to_bare().to_string();

    // A rolled-back route transition must not leak closed state.
    let mut rollback = c.fixture.admin.begin().await.expect("rollback tx");
    sqlx::query(&format!(
        "INSERT INTO {}.sm_custody_recipient_route_guards (recipient) VALUES ($1) ON CONFLICT DO NOTHING",
        c.fixture.target
    ))
    .bind(&recipient)
    .execute(&mut *rollback)
    .await
    .expect("seed route row");
    sqlx::query(&format!(
        "UPDATE {}.sm_custody_recipient_route_guards SET state = 1, revision = revision + 1 WHERE recipient = $1",
        c.fixture.target
    ))
    .bind(&recipient)
    .execute(&mut *rollback)
    .await
    .expect("tentative close");
    rollback.rollback().await.expect("rollback close");

    let mut sources = vec![first, second];
    sources.sort_by_key(|(frame, _)| frame.key.message_key.to_storage());
    let low = sources.remove(0);
    let high = sources.remove(0);
    let (high_result, low_result) = tokio::time::timeout(Duration::from_secs(5), async {
        // Deliberately start in reverse canonical-key order. Each source lock
        // is followed by the same recipient guard, so neither transaction
        // later asks for the other's source row.
        tokio::join!(repo.admit(&high.0, &high.1), repo.admit(&low.0, &low.1))
    })
    .await
    .expect("reverse-order source writes complete");
    high_result.expect("higher-key source admission");
    low_result.expect("lower-key source admission");

    let (guards, operations, state): (i64, i64, i16) = sqlx::query_as(&format!(
        "SELECT (SELECT count(*) FROM {}.sm_custody_recipient_route_guards WHERE recipient = $1), (SELECT count(*) FROM {}.sm_custody_operations), (SELECT state FROM {}.sm_custody_recipient_route_guards WHERE recipient = $1)",
        c.fixture.target, c.fixture.target, c.fixture.target
    ))
    .bind(&recipient)
    .fetch_one(&c.fixture.admin)
    .await
    .expect("route invariants");
    assert_eq!((guards, operations, state), (1, 2, 0));

    drop(repo);
    c.fixture.cleanup().await;
}

async fn wait_for_blocker(admin: &sqlx::PgPool, blocker: i32) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            let waiting: bool = sqlx::query_scalar(
                "SELECT EXISTS (SELECT 1 FROM pg_catalog.pg_stat_activity WHERE $1 = ANY(pg_catalog.pg_blocking_pids(pid)) AND query LIKE '%sm_custody_recipient_route_guards%')",
            )
            .bind(blocker)
            .fetch_one(admin)
            .await
            .expect("observe route guard waiter");
            if waiting {
                break;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("candidate writer waits for recipient route lock");
}
