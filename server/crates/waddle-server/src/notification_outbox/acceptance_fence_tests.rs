use super::*;
use crate::ingress::test_support::IngressFixture;
use crate::notification_outbox::test_support::{candidate, enqueue_jobs_for_test, target};

async fn claimed(fixture: &IngressFixture) -> (NotificationOutboxStore, NotificationOutboxJob) {
    let store = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("store");
    enqueue_jobs_for_test(&store, &candidate("acceptance-fence"), &[target()]).await;
    let job = store
        .claim_due_outbox_jobs(1)
        .await
        .expect("claim")
        .remove(0);
    assert!(!job.queue_acceptance_may_exist);
    (store, job)
}

async fn reclaim(
    store: &NotificationOutboxStore,
    job: &NotificationOutboxJob,
) -> NotificationOutboxJob {
    store
        .execute(
            "UPDATE notification_outbox SET claimed_at_ms = 1 WHERE job_id = ?",
            crate::db_params![job.job_id.as_str()],
        )
        .await
        .expect("stale claim");
    assert!(store
        .claim_due_outbox_jobs(1)
        .await
        .expect("schedule expired claim backoff")
        .is_empty());
    store
        .execute(
            "UPDATE notification_outbox SET next_attempt_at_ms = 1 WHERE job_id = ?",
            crate::db_params![job.job_id.as_str()],
        )
        .await
        .expect("advance retry deadline");
    store
        .claim_due_outbox_jobs(1)
        .await
        .expect("reclaim")
        .remove(0)
}

async fn stale_claim_neither_enters_nor_clears_boundary(fixture: IngressFixture) {
    let (store, stale) = claimed(&fixture).await;
    let fresh = reclaim(&store, &stale).await;
    assert_eq!(
        store
            .begin_queue_acceptance(&stale)
            .await
            .expect("stale boundary"),
        None
    );
    assert_eq!(
        store
            .begin_queue_acceptance(&fresh)
            .await
            .expect("fresh boundary"),
        Some(false)
    );
    assert_eq!(
        store
            .begin_queue_acceptance(&fresh)
            .await
            .expect("same-claim duplicate"),
        None
    );
    store
        .record_known_queue_refusal(&stale, false)
        .await
        .expect("stale refusal");
    assert_eq!(
        fixture
            .count("notification_outbox WHERE queue_acceptance_may_exist = 1")
            .await,
        1
    );
    assert!(!store
        .suppress_claimed_job(&stale)
        .await
        .expect("stale suppression"));
    assert!(!store
        .mark_job_failed(&stale, "stale refusal")
        .await
        .expect("stale failure"));
    assert!(matches!(
        store
            .fail_claimed_job_for_gone_registration(&stale, true)
            .await
            .expect("stale gone target"),
        NotificationOutboxPublishOutcome::RetryScheduled { .. }
    ));
    assert_eq!(
        store
            .schedule_retry_or_fail(&stale, "stale failure".into())
            .await
            .expect("stale retry"),
        None
    );
    assert_eq!(
        fixture
            .count("notification_outbox WHERE status = 'in-progress' AND attempt_count = 1")
            .await,
        1
    );
    // The current first invocation's definite refusal is a positive control.
    store
        .record_known_queue_refusal(&fresh, false)
        .await
        .expect("current refusal");
    assert_eq!(
        fixture
            .count("notification_outbox WHERE queue_acceptance_may_exist = 0")
            .await,
        1
    );
    assert!(store
        .suppress_claimed_job(&fresh)
        .await
        .expect("known unaccepted suppression"));
    drop(store);
    fixture.close().await;
}

async fn cancelled_boundary_retains_uncertainty_without_a_local_queue(fixture: IngressFixture) {
    let (store, first) = claimed(&fixture).await;
    assert_eq!(
        store
            .begin_queue_acceptance(&first)
            .await
            .expect("boundary"),
        Some(false)
    );
    // Crash/cancel after the boundary commit, before any downstream call. A
    // restarted owner cannot distinguish this from a lost queue commit reply.
    let fresh = reclaim(&store, &first).await;
    assert!(fresh.queue_acceptance_may_exist);
    assert_eq!(
        store
            .begin_queue_acceptance(&fresh)
            .await
            .expect("new boundary"),
        Some(true)
    );
    store
        .record_known_queue_refusal(&fresh, true)
        .await
        .expect("later known refusal");
    store
        .record_known_queue_refusal(&first, false)
        .await
        .expect("late first refusal");
    assert!(!store
        .suppress_claimed_job(&fresh)
        .await
        .expect("uncertain suppression"));
    assert!(!store
        .mark_job_failed(&fresh, "later known refusal")
        .await
        .expect("uncertain failure"));
    fixture
        .execute(
            "UPDATE notification_outbox SET attempt_count = 4 WHERE job_id = ?",
            crate::db_params![fresh.job_id.as_str()],
        )
        .await;
    let mut fresh = fresh;
    fresh.attempt_count = 4;
    assert_eq!(
        store
            .schedule_retry_or_fail(&fresh, "later refusal".into())
            .await
            .expect("unknown retry"),
        Some(5)
    );
    assert_eq!(fixture.count("notification_outbox WHERE status = 'failed' AND attempt_count = 5 AND queue_acceptance_may_exist = 1 AND claim_token IS NULL").await, 1);
    assert!(store
        .claim_due_outbox_jobs(16)
        .await
        .expect("parked jobs stay unclaimed")
        .is_empty());
    let pruned = store
        .prune_completed_before(i64::MAX, 16)
        .await
        .expect("prune");
    assert_eq!(
        pruned.jobs_deleted, 0,
        "uncertain parked evidence must survive retention"
    );
    drop(store);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_stale_claim_cannot_enter_or_clear_queue_boundary() {
    stale_claim_neither_enters_nor_clears_boundary(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_stale_claim_cannot_enter_or_clear_queue_boundary() {
    if let Some(fixture) = IngressFixture::postgres("queue_acceptance_fence").await {
        stale_claim_neither_enters_nor_clears_boundary(fixture).await;
    }
}
#[tokio::test]
async fn sqlite_cancellation_after_boundary_keeps_uncertain_custody() {
    cancelled_boundary_retains_uncertainty_without_a_local_queue(IngressFixture::sqlite().await)
        .await;
}
#[tokio::test]
async fn postgres_cancellation_after_boundary_keeps_uncertain_custody() {
    if let Some(fixture) = IngressFixture::postgres("queue_acceptance_cancel").await {
        cancelled_boundary_retains_uncertainty_without_a_local_queue(fixture).await;
    }
}

async fn legacy_unknown_work_cannot_coalesce_new_obligations(fixture: IngressFixture) {
    let store = NotificationOutboxStore::try_new(fixture.db.clone())
        .await
        .expect("store");
    enqueue_jobs_for_test(&store, &candidate("first-archive"), &[target()]).await;
    enqueue_jobs_for_test(&store, &candidate("second-archive"), &[target()]).await;
    let known = store
        .pending_outbox_jobs()
        .await
        .expect("coalesced known work");
    assert_eq!(known.len(), 1, "known never-started work still coalesces");
    assert_eq!(known[0].message_count, 2);
    assert!(!known[0].queue_acceptance_may_exist);
    fixture
        .execute(
            "UPDATE notification_outbox SET queue_acceptance_may_exist = 1 WHERE job_id = ?",
            crate::db_params![known[0].job_id.as_str()],
        )
        .await;
    let old = store
        .pending_outbox_jobs()
        .await
        .expect("legacy unknown")
        .remove(0);
    assert!(old.approved_payload.is_none());
    enqueue_jobs_for_test(&store, &candidate("third-archive"), &[target()]).await;
    let jobs = store.pending_outbox_jobs().await.expect("separate custody");
    assert_eq!(
        jobs.len(),
        2,
        "unknown legacy work neither mutates nor blocks a fresh obligation"
    );
    assert_eq!(
        jobs.iter()
            .find(|job| job.job_id == old.job_id)
            .expect("old job"),
        &old,
        "context, sender set, count, summary, payload and options remain immutable"
    );
    let new = jobs
        .iter()
        .find(|job| job.job_id != old.job_id)
        .expect("new job");
    assert_eq!(new.message_count, 1);
    assert!(!new.queue_acceptance_may_exist);
    assert_eq!(new.context.attr("stanza-id"), Some("third-archive"));
    drop(store);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_unknown_legacy_job_cannot_coalesce_new_work() {
    legacy_unknown_work_cannot_coalesce_new_obligations(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_unknown_legacy_job_cannot_coalesce_new_work() {
    if let Some(fixture) = IngressFixture::postgres("legacy_queue_coalescing").await {
        legacy_unknown_work_cannot_coalesce_new_obligations(fixture).await;
    }
}

#[tokio::test]
async fn postgres_queue_boundary_committing_while_cap_waits_keeps_custody() {
    let telemetry = waddle_xmpp::telemetry::test_support::acquire().await;
    let Some(fixture) = IngressFixture::postgres("queue_cap_boundary_race").await else {
        return;
    };
    let (store, mut job) = claimed(&fixture).await;
    fixture
        .execute(
            "UPDATE notification_outbox SET attempt_count = 4 WHERE job_id = ?",
            crate::db_params![job.job_id.as_str()],
        )
        .await;
    job.attempt_count = 4;
    let mut boundary = fixture.db.begin().await.expect("boundary transaction");
    boundary.execute("UPDATE notification_outbox SET queue_acceptance_may_exist = 1 WHERE job_id = ? AND claim_token = ?", crate::db_params![job.job_id.as_str(), job.claim_token.as_deref()]).await.expect("boundary marker before commit");
    let mut rows = boundary
        .query("SELECT txid_current()::text", ())
        .await
        .expect("boundary transaction id");
    let xid: String = rows
        .next()
        .await
        .expect("row")
        .expect("xid")
        .get(0)
        .expect("xid value");
    drop(rows);
    let retry_store = store.clone();
    let retry_job = job.clone();
    let retry = tokio::spawn(async move {
        retry_store
            .schedule_retry_or_fail(&retry_job, "known refusal at cap".into())
            .await
    });
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let conn = fixture.db.guard().await.expect("waiter connection");
            let mut rows = conn.query("SELECT COUNT(*) FROM pg_locks WHERE locktype = 'transactionid' AND transactionid::text = ? AND NOT granted", crate::db_params![xid.as_str()]).await.expect("cap waiter");
            let waiting: i64 = rows.next().await.expect("row").expect("count").get(0).expect("waiting count");
            if waiting > 0 { break; }
            drop(rows);
            drop(conn);
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    }).await.expect("cap waits for the boundary transaction's current uncertainty marker");
    boundary
        .commit()
        .await
        .expect("possible acceptance becomes durable");
    assert_eq!(
        retry.await.expect("retry task").expect("fenced retry"),
        Some(5),
        "the cap parks using the current uncertainty marker after acquiring the row lock"
    );
    assert_eq!(fixture.count("notification_outbox WHERE status = 'failed' AND attempt_count = 5 AND queue_acceptance_may_exist = 1 AND claim_token IS NULL").await, 1);
    assert_eq!(
        telemetry.counter_sum("xmpp.push.disposition", &[("disposition", "parked")]),
        Some(1)
    );
    assert_eq!(
        telemetry
            .counter_sum("xmpp.push.outbox_dead_lettered", &[])
            .unwrap_or(0),
        0
    );
    drop(store);
    fixture.close().await;
}
