use super::*;
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
