use super::*;
use crate::notification_outbox::test_support::*;

async fn retry_deadline(store: &NotificationOutboxStore, job: &NotificationOutboxJob) -> i64 {
    let mut rows = store
        .query(
            "SELECT next_attempt_at_ms FROM notification_outbox WHERE job_id = ?",
            crate::db_params![job.job_id.as_str()],
        )
        .await
        .expect("retry deadline");
    rows.next()
        .await
        .expect("row")
        .expect("job")
        .get(0)
        .expect("deadline")
}

#[tokio::test]
async fn durable_acceptance_marker_selects_the_hour_scale_retry_window() {
    for (uncertain, bounds) in [
        (
            false,
            [
                (3_750, 6_250),
                (7_500, 12_500),
                (15_000, 25_000),
                (30_000, 50_000),
            ],
        ),
        (
            true,
            [
                (225_000, 375_000),
                (450_000, 750_000),
                (900_000, 1_500_000),
                (1_800_000, 3_000_000),
            ],
        ),
    ] {
        let store = store().await;
        enqueue_jobs_for_test(&store, &candidate("backoff-window"), &[target()]).await;
        for (lower, upper) in bounds {
            let job = store
                .claim_due_outbox_jobs(1)
                .await
                .expect("claim")
                .remove(0);
            if uncertain {
                // On the first attempt the caller snapshot still says false.
                // Only the locked durable marker may choose the longer window.
                store.execute("UPDATE notification_outbox SET queue_acceptance_may_exist = 1 WHERE job_id = ?", crate::db_params![job.job_id.as_str()]).await.expect("persist uncertainty");
            }
            let before = crate::time::now_ms();
            store
                .schedule_retry_or_fail_with_failure(&job, PublishFailureClass::QueueRefused)
                .await
                .expect("retry")
                .expect("current claim transition");
            let after = crate::time::now_ms();
            let deadline = retry_deadline(&store, &job).await;
            assert!(
                deadline >= before + lower && deadline <= after + upper,
                "durable uncertainty={uncertain}: deadline {deadline} outside expected [{}, {}]",
                before + lower,
                after + upper
            );
            store
                .execute(
                    "UPDATE notification_outbox SET next_attempt_at_ms = 1 WHERE job_id = ?",
                    crate::db_params![job.job_id.as_str()],
                )
                .await
                .expect("advance to next attempt");
        }
    }
}

#[tokio::test]
async fn uncertain_policy_deferral_uses_the_combined_failure_count() {
    for (uncertain, lower, upper) in [(false, 45_000, 75_000), (true, 900_000, 1_500_000)] {
        let store = store().await;
        enqueue_jobs_for_test(&store, &candidate("policy-backoff-window"), &[target()]).await;
        let mut job = store
            .claim_due_outbox_jobs(1)
            .await
            .expect("claim")
            .remove(0);
        store.execute("UPDATE notification_outbox SET attempt_count = 2, queue_acceptance_may_exist = ? WHERE job_id = ?", crate::db_params![i64::from(uncertain), job.job_id.as_str()]).await.expect("prior failures");
        job.attempt_count = 2;
        let before = crate::time::now_ms();
        store
            .defer_claimed_job_without_attempt(&job, PublishFailureClass::PolicyLookup)
            .await
            .expect("policy deferral");
        let after = crate::time::now_ms();
        let deadline = retry_deadline(&store, &job).await;
        assert!(
            deadline >= before + lower && deadline <= after + upper,
            "uncertainty={uncertain}: third combined failure has wrong deadline"
        );
    }
}

#[test]
fn uncertain_backoff_is_capped_after_jitter() {
    for count in 1..=32 {
        for _ in 0..50 {
            assert!(uncertain_retry_delay_ms(count) <= 3_600_000);
        }
    }
}

#[tokio::test]
async fn claim_loss_uses_durable_uncertainty_without_reclassifying_later_refusal_or_policy() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    enqueue_jobs_for_test(&store, &candidate("sticky-attempt-classes"), &[target()]).await;
    let fresh = store
        .claim_due_outbox_jobs(1)
        .await
        .expect("claim")
        .remove(0);
    assert!(!fresh.queue_acceptance_may_exist);
    store
        .execute(
            "UPDATE notification_outbox SET queue_acceptance_may_exist = 1 WHERE job_id = ?",
            crate::db_params![fresh.job_id.as_str()],
        )
        .await
        .expect("cross queue boundary");
    store
        .schedule_retry_or_fail_with_failure(&fresh, PublishFailureClass::ClaimLost)
        .await
        .expect("lost claim after boundary")
        .expect("current claim transition");
    store
        .execute("UPDATE notification_outbox SET next_attempt_at_ms = 1", ())
        .await
        .expect("advance");
    let refused = store
        .claim_due_outbox_jobs(1)
        .await
        .expect("claim")
        .remove(0);
    store
        .schedule_retry_or_fail_with_failure(&refused, PublishFailureClass::QueueRefused)
        .await
        .expect("later refusal")
        .expect("current claim transition");
    store
        .execute("UPDATE notification_outbox SET next_attempt_at_ms = 1", ())
        .await
        .expect("advance");
    let policy = store
        .claim_due_outbox_jobs(1)
        .await
        .expect("claim")
        .remove(0);
    store
        .defer_claimed_job_without_attempt(&policy, PublishFailureClass::PolicyLookup)
        .await
        .expect("later pre-send policy failure");
    let known_store = crate::notification_outbox::test_support::store().await;
    enqueue_jobs_for_test(
        &known_store,
        &candidate("known-unaccepted-claim-loss"),
        &[target()],
    )
    .await;
    let known = known_store
        .claim_due_outbox_jobs(1)
        .await
        .expect("known claim")
        .remove(0);
    known_store
        .schedule_retry_or_fail_with_failure(&known, PublishFailureClass::ClaimLost)
        .await
        .expect("claim loss before boundary")
        .expect("current claim transition");
    assert_eq!(
        metrics.counter_sum(
            "xmpp.push.failure_class",
            &[("failure_class", "unknown_after_send")]
        ),
        Some(1)
    );
    assert_eq!(
        metrics.counter_sum(
            "xmpp.push.failure_class",
            &[("failure_class", "proven_refusal")]
        ),
        Some(1)
    );
    assert_eq!(
        metrics.counter_sum("xmpp.push.failure_class", &[("failure_class", "pre_send")]),
        Some(2)
    );
    let mut rows = store
        .query(
            "SELECT queue_acceptance_may_exist FROM notification_outbox",
            (),
        )
        .await
        .expect("custody");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("job")
            .get::<i64>(0)
            .expect("marker"),
        1
    );
}

#[tokio::test]
async fn explicit_retirement_preserves_settled_history_but_closes_unresolved_lineage() {
    let store = store().await;
    enqueue_jobs_for_test(
        &store,
        &candidate("retirement-history"),
        &[
            target_named("settled"),
            target_named("unresolved"),
            target_named("active"),
        ],
    )
    .await;
    let history_job = store
        .claim_due_outbox_jobs(3)
        .await
        .expect("claim fixture jobs")
        .into_iter()
        .find(|job| job.node.as_str() == "settled")
        .expect("historical job");
    assert!(store
        .mark_job_failed_with_failure(&history_job, PublishFailureClass::QueueRefused)
        .await
        .expect("settle historical failure"));
    store.execute("UPDATE notification_outbox SET last_error = 'historical-refusal', updated_at_ms = 41 WHERE node = 'settled'", ()).await.expect("historical diagnosis and tail");
    store.execute("UPDATE notification_outbox SET status = 'failed', last_error = 'known-refusal', claimed_at_ms = NULL, claim_token = NULL, updated_at_ms = 42 WHERE node = 'unresolved'", ()).await.expect("unresolved failure");
    let mut rows = store.query("SELECT COUNT(*) FROM notification_outbox_lineage JOIN notification_outbox USING (job_id) WHERE notification_outbox.node = 'unresolved' AND settled_at_ms IS NULL", ()).await.expect("outstanding lineage");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("count")
            .get::<i64>(0)
            .expect("count"),
        1
    );
    drop(rows);
    let mut tx = store.db.begin_immediate().await.expect("retirement");
    let retired = retire_registration_outbox_tx(
        &mut tx,
        &bare("alice@example.com"),
        Some(&bare("push.example.com")),
        None,
    )
    .await
    .expect("explicit retirement");
    assert_eq!(retired, 2, "count actual outstanding owners only");
    tx.commit().await.expect("commit");
    let mut rows = store
        .query(
            "SELECT last_error, updated_at_ms FROM notification_outbox WHERE node = 'settled'",
            (),
        )
        .await
        .expect("history");
    let history = rows.next().await.expect("row").expect("historical job");
    assert_eq!(
        history.get::<String>(0).expect("cause"),
        "historical-refusal"
    );
    assert_eq!(history.get::<i64>(1).expect("retention tail"), 41);
    let mut rows = store.query("SELECT COUNT(*) FROM notification_outbox WHERE node IN ('unresolved', 'active') AND status = 'failed' AND last_error = 'registration-revoked'", ()).await.expect("retired work");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("count")
            .get::<i64>(0)
            .expect("count"),
        2
    );
    let mut rows = store
        .query(
            "SELECT COUNT(*) FROM notification_outbox_lineage WHERE settled_at_ms IS NULL",
            (),
        )
        .await
        .expect("custody");
    assert_eq!(
        rows.next()
            .await
            .expect("row")
            .expect("count")
            .get::<i64>(0)
            .expect("count"),
        0
    );
}
