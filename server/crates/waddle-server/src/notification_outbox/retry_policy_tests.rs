use super::{
    retire_registration_outbox_tx, uncertain_retry_delay_ms, NotificationOutboxJob,
    NotificationOutboxPublishOutcome, NotificationOutboxStatus, NotificationOutboxStore,
    PublishFailureClass, STATUS_FAILED, STATUS_QUEUED,
};
use crate::notification_outbox::test_support::{
    bare, candidate, enqueue_jobs_for_test, inbox_with_unread, store, target, target_named,
};

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
async fn terminal_proof_lookup_failure_is_pre_send_storage_without_queue_acceptance() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let store = store().await;
    let push = crate::push_service::DatabasePushServiceStore::new_with_secret_key(
        store.db.clone(),
        &rand::random::<[u8; 32]>(),
    )
    .await
    .expect("provider store");
    enqueue_jobs_for_test(&store, &candidate("proof-lookup-storage"), &[target()]).await;
    let queued = store.pending_outbox_jobs().await.expect("queued").remove(0);
    let payload = queued.to_xep0357_pubsub_item().payload.expect("payload");
    store
        .execute(
            "UPDATE notification_outbox SET approved_payload_xml = ? WHERE job_id = ?",
            crate::db_params![String::from(&payload), queued.job_id.as_str()],
        )
        .await
        .expect("frozen prior approval");
    // Keep the real provider query boundary, while making only that lookup fail.
    store
        .execute(
            "ALTER TABLE push_publish_jobs RENAME TO unavailable_publish_jobs",
            (),
        )
        .await
        .expect("provider storage unavailable");
    let inbox = inbox_with_unread(&queued.recipient_bare_jid, &queued.conversation_jid, 1).await;
    let outcomes = store
        .drain_due_outbox_jobs(
            &push,
            &waddle_xmpp::push::InMemoryPushStore::new(),
            &inbox,
            &waddle_xmpp::xep::xep0191::InMemoryBlockingStorage::new(),
            &bare("push.example.com"),
            1,
        )
        .await
        .expect("storage failure schedules retry");
    assert!(matches!(
        outcomes.as_slice(),
        [NotificationOutboxPublishOutcome::RetryScheduled { .. }]
    ));
    assert_eq!(
        metrics.counter_sum("xmpp.push.outbox_retry_scheduled", &[("reason", "storage")]),
        Some(1)
    );
    assert_eq!(
        metrics.counter_sum("xmpp.push.failure_class", &[("failure_class", "pre_send")]),
        Some(1)
    );
    assert_eq!(
        metrics
            .counter_sum(
                "xmpp.push.failure_class",
                &[("failure_class", "unknown_after_send")]
            )
            .unwrap_or(0),
        0
    );
    let mut rows = store.query("SELECT queue_acceptance_may_exist, approved_payload_xml, (SELECT COUNT(*) FROM unavailable_publish_jobs) FROM notification_outbox", ()).await.expect("queue unchanged");
    let row = rows.next().await.expect("row").expect("job");
    assert_eq!(row.get::<i64>(0).expect("acceptance marker"), 0);
    assert_eq!(
        row.get::<String>(1).expect("frozen payload"),
        String::from(&payload)
    );
    assert_eq!(row.get::<i64>(2).expect("provider jobs"), 0);
}

#[tokio::test]
async fn expired_claims_persist_backoff_and_consume_budget_before_reclaim() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    for uncertain in [false, true] {
        let store = store().await;
        enqueue_jobs_for_test(&store, &candidate("expired-backoff"), &[target()]).await;
        let mut job = store
            .claim_due_outbox_jobs(1)
            .await
            .expect("first claim")
            .remove(0);
        let payload = job.to_xep0357_pubsub_item().payload.expect("payload");
        store.execute("UPDATE notification_outbox SET queue_acceptance_may_exist = ?, approved_payload_xml = ? WHERE job_id = ?", crate::db_params![i64::from(uncertain), String::from(&payload), job.job_id.as_str()]).await.expect("durable boundary");
        for count in 1..=5 {
            store
                .execute(
                    "UPDATE notification_outbox SET claimed_at_ms = 1 WHERE job_id = ?",
                    crate::db_params![job.job_id.as_str()],
                )
                .await
                .expect("expired claim");
            let before = crate::time::now_ms();
            assert!(store
                .claim_due_outbox_jobs(1)
                .await
                .expect("expire without replay")
                .is_empty());
            let after = crate::time::now_ms();
            let mut rows = store.query("SELECT status, attempt_count, next_attempt_at_ms, claim_token, queue_acceptance_may_exist, approved_payload_xml FROM notification_outbox WHERE job_id = ?", crate::db_params![job.job_id.as_str()]).await.expect("persisted transition");
            let row = rows.next().await.expect("row").expect("job");
            assert_eq!(row.get::<i64>(1).expect("attempts"), count);
            assert!(row.get::<Option<String>>(3).expect("token").is_none());
            assert_eq!(
                row.get::<i64>(4).expect("uncertainty"),
                i64::from(uncertain)
            );
            assert_eq!(
                row.get::<String>(5).expect("frozen payload"),
                String::from(&payload)
            );
            if count == 5 {
                assert_eq!(row.get::<String>(0).expect("status"), STATUS_FAILED);
                assert!(row.get::<Option<i64>>(2).expect("deadline").is_none());
            } else {
                assert_eq!(row.get::<String>(0).expect("status"), STATUS_QUEUED);
                let delay = if uncertain { 300_000 } else { 5_000 } * (1_i64 << (count - 1));
                let deadline = row.get::<i64>(2).expect("deadline");
                assert!(deadline >= before + delay * 3 / 4 && deadline <= after + delay * 5 / 4);
            }
            drop(rows);
            assert_eq!(
                store
                    .schedule_retry_or_fail_with_failure(&job, PublishFailureClass::ClaimLost)
                    .await
                    .expect("old token fenced"),
                None
            );
            assert!(store
                .claim_due_outbox_jobs(1)
                .await
                .expect("no early reclaim")
                .is_empty());
            if count < 5 {
                store
                    .execute(
                        "UPDATE notification_outbox SET next_attempt_at_ms = 1 WHERE job_id = ?",
                        crate::db_params![job.job_id.as_str()],
                    )
                    .await
                    .expect("advance retry deadline");
                let fresh = store
                    .claim_due_outbox_jobs(1)
                    .await
                    .expect("due retry")
                    .remove(0);
                assert_ne!(fresh.claim_token, job.claim_token);
                assert_eq!(fresh.attempt_count, count);
                job = fresh;
            }
        }
        let mut rows = store
            .query(
                "SELECT COUNT(*) FROM notification_outbox_lineage WHERE settled_at_ms IS NULL",
                (),
            )
            .await
            .expect("retained custody");
        assert_eq!(
            rows.next()
                .await
                .expect("row")
                .expect("count")
                .get::<i64>(0)
                .expect("unsettled"),
            i64::from(uncertain)
        );
        drop(rows);
        store
            .execute("UPDATE notification_outbox SET updated_at_ms = 1", ())
            .await
            .expect("expire terminal history");
        store.execute("UPDATE notification_outbox_lineage SET settled_at_ms = 1 WHERE settled_at_ms IS NOT NULL", ()).await.expect("expire settled lineage tail");
        assert_eq!(
            store
                .prune_completed_before(i64::MAX, 1)
                .await
                .expect("retention")
                .jobs_deleted,
            u64::from(!uncertain)
        );
    }
    assert_eq!(
        metrics.counter_sum(
            "xmpp.push.failure_class",
            &[("failure_class", "unknown_after_send")]
        ),
        Some(5)
    );
    assert_eq!(
        metrics.counter_sum("xmpp.push.failure_class", &[("failure_class", "pre_send")]),
        Some(5)
    );
}

#[tokio::test]
async fn expired_claim_without_token_still_schedules_a_fenced_retry() {
    let store = store().await;
    enqueue_jobs_for_test(&store, &candidate("expired-null-token"), &[target()]).await;
    let job = store
        .claim_due_outbox_jobs(1)
        .await
        .expect("claim")
        .remove(0);
    store
        .execute(
            "UPDATE notification_outbox SET claim_token = NULL, claimed_at_ms = 1 WHERE job_id = ?",
            crate::db_params![job.job_id.as_str()],
        )
        .await
        .expect("legacy missing token");
    let tokenless = store
        .pending_outbox_jobs()
        .await
        .expect("legacy claim snapshot")
        .remove(0);
    assert!(store
        .claim_due_outbox_jobs(1)
        .await
        .expect("expire legacy claim")
        .is_empty());
    let pending = store.pending_outbox_jobs().await.expect("pending");
    assert_eq!(pending[0].status, NotificationOutboxStatus::Queued);
    assert_eq!(pending[0].attempt_count, 1);
    assert!(pending[0].claim_token.is_none());
    assert!(retry_deadline(&store, &job).await > crate::time::now_ms());
    assert_eq!(
        store
            .schedule_retry_or_fail_with_failure(&job, PublishFailureClass::ClaimLost)
            .await
            .expect("former token fenced"),
        None
    );
    store
        .execute(
            "UPDATE notification_outbox SET next_attempt_at_ms = 1 WHERE job_id = ?",
            crate::db_params![job.job_id.as_str()],
        )
        .await
        .expect("advance retry deadline");
    let fresh = store
        .claim_due_outbox_jobs(1)
        .await
        .expect("claim successor")
        .remove(0);
    assert!(fresh.claim_token.is_some());
    assert_eq!(
        store
            .schedule_retry_or_fail_with_failure(&tokenless, PublishFailureClass::ClaimLost)
            .await
            .expect("missing token cannot mutate successor"),
        None
    );
    assert_eq!(
        store
            .pending_outbox_jobs()
            .await
            .expect("unchanged successor"),
        vec![fresh]
    );
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
