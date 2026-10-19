use super::*;
use crate::push_service::test_support::{notification_item, owner, scalar_i64, store};

#[tokio::test]
async fn legacy_queued_work_past_budget_parks_before_provider_send() {
    use crate::push_service::test_support::{
        fcm_registration, store_with_fake_fcm, FCM_TEST_APP_ID,
    };
    let (store, sender) = store_with_fake_fcm().await;
    let node = store
        .ensure_node(&owner(), FCM_TEST_APP_ID)
        .await
        .expect("node");
    store
        .upsert_device(&owner(), fcm_registration("device", node.node()))
        .await
        .expect("device");
    let accepted = store
        .enqueue_notification_publish_job_from_user_server(
            node.node(),
            &notification_item("legacy-budget"),
            &owner(),
        )
        .await
        .expect("accepted");
    let id = accepted.job_id().to_string();
    store
        .execute(
            "UPDATE push_publish_jobs SET attempt_count = 50, uncertain_send = 1 WHERE job_id = ?",
            crate::db_params![&id],
        )
        .await
        .expect("legacy exhausted queued work");
    assert!(store
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("drain")
        .is_empty());
    assert_eq!(sender.sent(), 0);
    let job = store
        .load_publish_job(&id)
        .await
        .expect("read")
        .expect("retained");
    assert_eq!(job.status(), PUBLISH_JOB_STATUS_FAILED);
    assert!(job.uncertain_send);
    assert_eq!(
        scalar_i64(
            &store,
            "SELECT attempt_count FROM push_publish_jobs WHERE job_id = ?",
            crate::db_params![&id]
        )
        .await,
        50
    );
}

#[tokio::test]
async fn parked_unknown_send_remains_unscheduled_after_restart() {
    let dir = tempfile::tempdir().expect("temporary database directory");
    let path = dir.path().join("parked.sqlite3");
    let key = [37; 32];
    let (id, node) = {
        let db = crate::db::Database::open_local("parked-restart", &path)
            .await
            .expect("database");
        let store = DatabasePushServiceStore::new_with_secret_key(db, &key)
            .await
            .expect("store");
        let node = store
            .ensure_node(&owner(), "parked-restart")
            .await
            .expect("node");
        let accepted = store
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("parked-restart"),
                &owner(),
            )
            .await
            .expect("accepted");
        let id = accepted.job_id().to_string();
        store.execute("UPDATE push_publish_jobs SET status = 'failed', uncertain_send = 1, attempt_count = ? WHERE job_id = ?", crate::db_params![PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS, &id]).await.expect("parked unknown acceptance");
        (id, node.node().to_string())
    };
    let db = crate::db::Database::open_local("parked-restarted", &path)
        .await
        .expect("reopened database");
    let store = DatabasePushServiceStore::new_with_secret_key(db, &key)
        .await
        .expect("restarted store");
    assert!(store
        .drain_queued_notification_publish_jobs(100)
        .await
        .expect("restart drain")
        .is_empty());
    assert_eq!(
        store
            .load_publish_job(&id)
            .await
            .expect("read")
            .expect("retained evidence")
            .status(),
        PUBLISH_JOB_STATUS_FAILED
    );
    store
        .disable_nodes_for_owner(&owner(), Some(&node))
        .await
        .expect("retire after restart");
    assert_eq!(scalar_i64(&store, "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ? AND terminal_disposition = 'registration-revoked' AND uncertain_send = 1", crate::db_params![&id]).await, 1);
}

#[tokio::test]
async fn expired_claims_consume_budget_and_park_without_reclaim() {
    let store = store().await;
    let node = store
        .ensure_node(&owner(), "expired-budget")
        .await
        .expect("node");
    let accepted = store
        .enqueue_notification_publish_job_from_user_server(
            node.node(),
            &notification_item("expired-budget"),
            &owner(),
        )
        .await
        .expect("accepted");
    let id = accepted.job_id().to_string();
    for attempt in 1..=PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS {
        let mut tx = store.db.begin_immediate().await.expect("claim transaction");
        let claim = claim_publish_job_tx(&mut tx, &id, i64::MAX)
            .await
            .expect("claim")
            .expect("eligible before budget");
        tx.commit().await.expect("claim commit");
        store
            .execute(
                "UPDATE push_publish_jobs SET claimed_at_ms = 0 WHERE job_id = ?",
                crate::db_params![&id],
            )
            .await
            .expect("lost worker");
        store
            .recover_stale_publish_job_claim_by_id(&id)
            .await
            .expect("recover expired claim");
        assert_eq!(
            scalar_i64(
                &store,
                "SELECT attempt_count FROM push_publish_jobs WHERE job_id = ?",
                crate::db_params![&id]
            )
            .await,
            attempt
        );
        assert!(
            store
                .load_publish_job(&id)
                .await
                .expect("read")
                .expect("retained")
                .uncertain_send
        );
        // A late result cannot clear a recovered/parked claim.
        let mut tx = store.db.begin_immediate().await.expect("late transaction");
        record_known_publish_job_failure_tx(&mut tx, &claim, "late refusal", crate::time::now_ms())
            .await
            .expect("fenced late result");
        tx.commit().await.expect("late commit");
    }
    let parked = store
        .load_publish_job(&id)
        .await
        .expect("read")
        .expect("parked retained");
    assert_eq!(parked.status(), PUBLISH_JOB_STATUS_FAILED);
    assert!(store
        .queued_publish_jobs()
        .await
        .expect("scheduled")
        .is_empty());
    let mut tx = store
        .db
        .begin_immediate()
        .await
        .expect("reclaim transaction");
    assert!(claim_publish_job_tx(&mut tx, &id, i64::MAX)
        .await
        .expect("reclaim")
        .is_none());
    tx.commit().await.expect("reclaim commit");
}

#[tokio::test]
async fn parked_evidence_survives_pruning_until_explicit_revocation_and_tail() {
    let store = store().await;
    let node = store
        .ensure_node(&owner(), "parked-gc")
        .await
        .expect("node");
    store
        .upsert_device(
            &owner(),
            crate::push_service::PushDeviceRegistration::new(
                "device",
                node.node(),
                crate::push_service::PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("device for attempt evidence");
    let accepted = store
        .enqueue_notification_publish_job_from_user_server(
            node.node(),
            &notification_item("parked-gc"),
            &owner(),
        )
        .await
        .expect("accepted");
    let id = accepted.job_id().to_string();
    let old = crate::time::now_ms()
        - crate::ingress_substrate::ALIAS_RETENTION.num_milliseconds()
        - 1_000;
    store.execute("UPDATE push_publish_jobs SET status = 'failed', uncertain_send = 1, updated_at_ms = ? WHERE job_id = ?", crate::db_params![old, &id]).await.expect("parked evidence");
    store.execute("INSERT INTO push_delivery_attempts (attempt_id, publish_job_id, node, device_id, platform, item_id, status, created_at_ms) VALUES (?, ?, ?, 'device', 'web', 'parked-gc', 'web-delivered', ?)", crate::db_params![uuid::Uuid::new_v4().to_string(), &id, node.node(), old]).await.expect("confirmed sibling evidence");
    let mut tx = store.db.begin_immediate().await.expect("prune transaction");
    prune_publish_jobs_tx(&mut tx, node.node(), 0)
        .await
        .expect("prune jobs");
    prune_delivery_attempts_tx(&mut tx, node.node(), 0)
        .await
        .expect("prune attempts");
    tx.commit().await.expect("prune commit");
    assert!(
        store.load_publish_job(&id).await.expect("read").is_some(),
        "age alone cannot dispose uncertain work"
    );
    assert_eq!(
        store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts")
            .len(),
        1
    );
    store
        .disable_nodes_for_owner(&owner(), Some(node.node()))
        .await
        .expect("authoritative retirement");
    assert_eq!(scalar_i64(&store, "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ? AND terminal_disposition = 'registration-revoked'", crate::db_params![&id]).await, 1);
    let mut tx = store.db.begin_immediate().await.expect("tail transaction");
    prune_publish_jobs_tx(&mut tx, node.node(), 0)
        .await
        .expect("tail protects");
    tx.commit().await.expect("tail commit");
    assert!(store.load_publish_job(&id).await.expect("read").is_some());
    store
        .execute(
            "UPDATE push_publish_jobs SET updated_at_ms = ? WHERE job_id = ?",
            crate::db_params![old, &id],
        )
        .await
        .expect("elapsed disposition tail");
    let mut tx = store.db.begin_immediate().await.expect("gc transaction");
    prune_publish_jobs_tx(&mut tx, node.node(), 0)
        .await
        .expect("eligible gc");
    tx.commit().await.expect("gc commit");
    assert!(store.load_publish_job(&id).await.expect("read").is_none());
}
