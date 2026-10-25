use super::{
    DatabasePushServiceStore, PushQueueAcceptanceError, XmppError, ALIAS_RETENTION,
    MAX_PUBLISH_JOBS_PER_NODE, PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS,
};
use crate::ingress::test_support::IngressFixture;
use crate::ingress_uow::{
    settle_recorded, CanonicalMessageRepository, EffectDescendantRepository, EffectIntentRepository,
};
use crate::push_service::test_support::{notification_item, owner, scalar_i64};
use waddle_xmpp::ingress::{
    IngressEffectIntent, MessageKey, NotificationActivityMutation, NotificationCandidateOutcome,
    SemanticDigest,
};

async fn quota_headroom_skips_protected_oldest_rows(fixture: IngressFixture) {
    let push = DatabasePushServiceStore::new_with_secret_key(
        fixture.db.clone(),
        &rand::random::<[u8; 32]>(),
    )
    .await
    .expect("push store");
    let node = push
        .ensure_node(&owner(), "quota-headroom")
        .await
        .expect("node");
    let template = push
        .enqueue_notification_publish_job_from_user_server(
            node.node(),
            &notification_item("quota-template"),
            &owner(),
        )
        .await
        .expect("template");
    let protected = [uuid::Uuid::new_v4(), uuid::Uuid::new_v4()];
    let prefix = uuid::Uuid::new_v4().to_string();
    let now = crate::time::now_ms();
    let old = now - crate::ingress_substrate::ALIAS_RETENTION.num_milliseconds() - 1_000;
    let mut tx = push.db.begin_immediate().await.expect("quota fixture");
    tx.execute(r#"
        WITH RECURSIVE ids(n) AS (SELECT 0 UNION ALL SELECT n + 1 FROM ids WHERE n < ?)
        INSERT INTO push_publish_jobs (job_id, owner_bare_jid, node, item_id, payload_xml, acceptance_scope, backing_state, publication_order, ancestry_job_id, status, attempt_count, uncertain_send, created_at_ms, updated_at_ms)
        SELECT ? || '-' || CAST(n AS TEXT), owner_bare_jid, node, ? || '-item-' || CAST(n AS TEXT), payload_xml,
          CASE WHEN n IN (1, 2) THEN 'canonical' ELSE 'wire' END, 'published', n + 1,
          CASE WHEN n = 1 THEN ? WHEN n = 2 THEN ? ELSE NULL END,
          CASE WHEN n = 0 THEN 'queued' ELSE 'failed' END, 0, CASE WHEN n = 0 THEN 1 ELSE 0 END,
          ? - ? + n, CASE WHEN n = 4 THEN ? ELSE ? END
        FROM push_publish_jobs CROSS JOIN ids WHERE job_id = ?
    "#, crate::db_params![MAX_PUBLISH_JOBS_PER_NODE - 1, &prefix, &prefix, protected[0].to_string(), protected[1].to_string(), old, MAX_PUBLISH_JOBS_PER_NODE, now, old, template.job_id().to_string()]).await.expect("full node queue");
    tx.execute(
        "DELETE FROM push_publish_jobs WHERE job_id = ?",
        crate::db_params![template.job_id().to_string()],
    )
    .await
    .expect("remove seed template");
    tx.execute(
        "UPDATE push_publication_orders SET next_order = ? WHERE node = ?",
        crate::db_params![MAX_PUBLISH_JOBS_PER_NODE, node.node()],
    )
    .await
    .expect("publication frontier");
    tx.commit().await.expect("quota fixture commit");
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: owner(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: "bob@example.com".parse().expect("conversation"),
            archive_stanza_id: waddle_xmpp_core::xep0359::StanzaId::new("quota", owner().into()),
            outcome: NotificationCandidateOutcome::Inserted,
        },
    };
    for (index, delivery) in protected.into_iter().enumerate() {
        let key = MessageKey::new();
        let mut tx = fixture
            .uow
            .begin()
            .await
            .expect("protected canonical parent");
        CanonicalMessageRepository::record_message(
            &mut tx,
            key,
            &SemanticDigest::from_storage(1, [71; 32]).expect("digest"),
            None,
        )
        .await
        .expect("parent");
        EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
            .await
            .expect("intent");
        EffectDescendantRepository::attach(&mut tx, key, &intent.semantic_key(), delivery)
            .await
            .expect("custody");
        settle_recorded(&mut tx, key, std::slice::from_ref(&intent))
            .await
            .expect("receipt");
        CanonicalMessageRepository::terminalize(
            &mut tx,
            key,
            chrono::Utc::now() - chrono::Duration::days(20),
        )
        .await
        .expect("terminal proof");
        if index == 0 {
            EffectDescendantRepository::settle_all(&mut tx, delivery, chrono::Utc::now())
                .await
                .expect("settled references still protect replay evidence");
        }
        tx.commit().await.expect("parent commit");
    }
    assert_eq!(
        scalar_i64(
            &push,
            "SELECT COUNT(*) FROM push_publish_jobs WHERE node = ?",
            crate::db_params![node.node()]
        )
        .await,
        MAX_PUBLISH_JOBS_PER_NODE
    );
    push.enqueue_notification_publish_job_from_user_server(
        node.node(),
        &notification_item("accepted-after-headroom"),
        &owner(),
    )
    .await
    .expect("later safe terminal history must free quota despite protected oldest rows");
    assert_eq!(
        scalar_i64(
            &push,
            "SELECT COUNT(*) FROM push_publish_jobs WHERE node = ?",
            crate::db_params![node.node()]
        )
        .await,
        MAX_PUBLISH_JOBS_PER_NODE
    );
    for index in [0, 1, 2, 4, 5] {
        assert_eq!(
            scalar_i64(
                &push,
                "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ?",
                crate::db_params![format!("{prefix}-{index}")]
            )
            .await,
            1,
            "live/uncertain, canonical, fresh and unneeded terminal rows survive"
        );
    }
    assert_eq!(
        scalar_i64(
            &push,
            "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ?",
            crate::db_params![format!("{prefix}-3")]
        )
        .await,
        0,
        "prune the oldest actually deletable row only"
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_quota_headroom_skips_protected_oldest_rows() {
    quota_headroom_skips_protected_oldest_rows(IngressFixture::sqlite().await).await;
}
#[tokio::test]
async fn postgres_quota_headroom_skips_protected_oldest_rows() {
    if let Some(fixture) = IngressFixture::postgres("publish_quota_headroom").await {
        quota_headroom_skips_protected_oldest_rows(fixture).await;
    }
}

async fn parked_uncertainty_backpressures_only_the_saturated_node(fixture: IngressFixture) {
    use crate::push_service::test_support::{
        fcm_registration, with_fake_fcm, FakeFcmSender, FCM_TEST_APP_ID,
    };

    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let sender = FakeFcmSender::default();
    let push = with_fake_fcm(
        DatabasePushServiceStore::new_with_secret_key(
            fixture.db.clone(),
            &rand::random::<[u8; 32]>(),
        )
        .await
        .expect("push store"),
        sender.clone(),
    );
    crate::push_registrations::DatabasePushRegistrationStore::try_new(fixture.db.clone())
        .await
        .expect("registration schema");
    let saturated = push
        .ensure_node(&owner(), "parked-capacity")
        .await
        .expect("saturated node");
    push.upsert_device(
        &owner(),
        fcm_registration("parked-device", saturated.node()),
    )
    .await
    .expect("saturated registration has an active device");
    push.register_first_party_node_for_owner(&owner(), "push.example.com", saturated.node(), None)
        .await
        .expect("registration");
    let delivery = uuid::Uuid::new_v4();
    let service = "push.example.com".parse().expect("service");
    let item = notification_item(&delivery.to_string());
    let accepted = push
        .enqueue_canonical_notification_publish_job(
            saturated.node(),
            &item,
            &owner(),
            &service,
            None,
            delivery,
        )
        .await
        .expect("canonical acceptance before saturation");
    let old = crate::time::now_ms()
        - crate::ingress_substrate::ALIAS_RETENTION.num_milliseconds()
        - 1_000;
    let prefix = uuid::Uuid::new_v4().to_string();
    let mut tx = push.db.begin_immediate().await.expect("parked fixture");
    tx.execute(
        "UPDATE push_publish_jobs SET status = 'failed', uncertain_send = 1, attempt_count = ?, created_at_ms = ?, updated_at_ms = ? WHERE job_id = ?",
        crate::db_params![PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS, old, old, accepted.job_id().to_string()],
    )
    .await
    .expect("park canonical work beyond its retry budget");
    tx.execute(
        r#"
        WITH RECURSIVE ids(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM ids WHERE n < ?)
        INSERT INTO push_publish_jobs (job_id, owner_bare_jid, node, item_id, payload_xml, acceptance_scope, backing_state, publication_order, status, attempt_count, uncertain_send, created_at_ms, updated_at_ms)
        SELECT ? || '-' || CAST(n AS TEXT), owner_bare_jid, node, ? || '-item-' || CAST(n AS TEXT), payload_xml,
          'wire', 'published', n + 1, 'failed', ?, 1, ?, ?
        FROM push_publish_jobs CROSS JOIN ids WHERE job_id = ?
        "#,
        crate::db_params![MAX_PUBLISH_JOBS_PER_NODE - 1, &prefix, &prefix, PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS, old, old, accepted.job_id().to_string()],
    )
    .await
    .expect("fill node with old parked uncertain work");
    tx.execute(
        "UPDATE push_publication_orders SET next_order = ? WHERE node = ?",
        crate::db_params![MAX_PUBLISH_JOBS_PER_NODE, saturated.node()],
    )
    .await
    .expect("publication frontier");
    tx.commit().await.expect("parked fixture commit");

    let refused = uuid::Uuid::new_v4();
    let result = push
        .enqueue_canonical_notification_publish_job(
            saturated.node(),
            &notification_item(&refused.to_string()),
            &owner(),
            &service,
            None,
            refused,
        )
        .await;
    assert!(matches!(
        result,
        Err(PushQueueAcceptanceError::KnownNotAccepted(
            XmppError::Stanza {
                condition: waddle_xmpp::StanzaErrorCondition::ResourceConstraint,
                error_type: waddle_xmpp::StanzaErrorType::Wait,
                ..
            }
        ))
    ));
    assert!(
        metrics
            .counter_sum("xmpp.push.failure_reason", &[("reason", "capacity")])
            .is_some_and(|count| count >= 1),
        "capacity pressure must be observable without database access"
    );
    assert!(metrics
        .counter_sum("xmpp.push.disposition", &[("disposition", "backpressure")])
        .is_some_and(|count| count >= 1));
    let replay = push
        .enqueue_canonical_notification_publish_job(
            saturated.node(),
            &item,
            &owner(),
            &service,
            None,
            delivery,
        )
        .await
        .expect("identical acceptance replay bypasses new-work admission");
    assert_eq!(replay.job_id(), accepted.job_id());
    assert!(!replay.queued(), "replay cannot resume parked work");

    let healthy = push
        .ensure_node(&owner(), FCM_TEST_APP_ID)
        .await
        .expect("healthy node");
    push.upsert_device(&owner(), fcm_registration("healthy-device", healthy.node()))
        .await
        .expect("healthy device");
    push.enqueue_notification_publish_job_from_user_server(
        healthy.node(),
        &notification_item("healthy-after-saturation"),
        &owner(),
    )
    .await
    .expect("another node retains admission capacity");
    let drained = push
        .drain_queued_notification_publish_jobs(16)
        .await
        .expect("healthy queue progresses");
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0].item_id(), "healthy-after-saturation");
    assert_eq!(
        sender.sent(),
        1,
        "parked work never re-enters provider dispatch"
    );
    assert_eq!(
        scalar_i64(
            &push,
            "SELECT COUNT(*) FROM push_publish_jobs WHERE node = ? AND status = 'failed' AND uncertain_send = 1 AND updated_at_ms = ?",
            crate::db_params![saturated.node(), old],
        )
        .await,
        MAX_PUBLISH_JOBS_PER_NODE,
        "admission, replay and draining preserve every old accepted obligation"
    );
    assert_eq!(
        scalar_i64(
            &push,
            "SELECT COUNT(*) FROM push_publish_jobs WHERE ancestry_job_id = ?",
            crate::db_params![refused.to_string()],
        )
        .await,
        0,
        "backpressure happens before canonical acceptance"
    );
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_parked_uncertainty_backpressures_only_the_saturated_node() {
    parked_uncertainty_backpressures_only_the_saturated_node(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_parked_uncertainty_backpressures_only_the_saturated_node() {
    if let Some(fixture) = IngressFixture::postgres("parked_push_capacity").await {
        parked_uncertainty_backpressures_only_the_saturated_node(fixture).await;
    }
}
