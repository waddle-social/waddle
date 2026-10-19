use super::*;
use crate::ingress::test_support::IngressFixture;
use crate::ingress_uow::{
    settle_recorded, CanonicalMessageRepository, EffectDescendantRepository, EffectIntentRepository,
};
use crate::push_service::test_support::{notification_item, owner, scalar_i64};
use crate::push_service::{PushDeviceRegistration, PushServiceNode};
use waddle_xmpp::ingress::{
    IngressEffectIntent, MessageKey, NotificationActivityMutation, NotificationCandidateOutcome,
    SemanticDigest,
};

struct State {
    push: DatabasePushServiceStore,
    node: PushServiceNode,
    job: uuid::Uuid,
    delivery: uuid::Uuid,
    service: BareJid,
}

async fn seed(fixture: &IngressFixture, label: &str) -> State {
    let service: BareJid = "push.example.com".parse().expect("service");
    crate::push_registrations::DatabasePushRegistrationStore::new(fixture.db.clone())
        .await
        .expect("registrations");
    let backing = Arc::new(
        crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
            .await
            .expect("backing"),
    );
    let push = DatabasePushServiceStore::new_with_secret_key_and_pubsub(
        fixture.db.clone(),
        &rand::random::<[u8; 32]>(),
        service.clone(),
        backing,
    )
    .await
    .expect("push store");
    let node = push.ensure_node(&owner(), label).await.expect("node");
    push.upsert_device(
        &owner(),
        PushDeviceRegistration::new("device", node.node(), PushDevicePlatform::Web, "test"),
    )
    .await
    .expect("device");
    push.register_first_party_node_for_owner(&owner(), service.as_str(), node.node(), None)
        .await
        .expect("registration");
    let delivery = uuid::Uuid::new_v4();
    let accepted = push
        .enqueue_canonical_notification_publish_job(
            node.node(),
            &notification_item(label),
            &owner(),
            &service,
            None,
            delivery,
        )
        .await
        .expect("canonical acceptance");
    let intent = IngressEffectIntent::NotificationActivityPreview {
        owner: owner(),
        mutation: NotificationActivityMutation::NotificationCandidate {
            conversation: "bob@example.com".parse().expect("conversation"),
            archive_stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(label, owner().into()),
            outcome: NotificationCandidateOutcome::Inserted,
        },
    };
    for _ in 0..2 {
        let key = MessageKey::new();
        let mut tx = fixture.uow.begin().await.expect("canonical parent");
        CanonicalMessageRepository::record_message(
            &mut tx,
            key,
            &SemanticDigest::from_storage(1, [63; 32]).expect("digest"),
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
            .expect("operational receipt");
        CanonicalMessageRepository::terminalize(
            &mut tx,
            key,
            chrono::Utc::now() - chrono::Duration::days(20),
        )
        .await
        .expect("old terminal proof");
        tx.commit().await.expect("parent commit");
    }
    State {
        push,
        node,
        job: accepted.job_id(),
        delivery,
        service,
    }
}

async fn claim(state: &State) -> PushPublishJob {
    state
        .push
        .execute(
            "UPDATE push_publish_jobs SET next_retry_at_ms = NULL WHERE job_id = ?",
            crate::db_params![state.job.to_string()],
        )
        .await
        .expect("retry now");
    let Phase1Outcome::Continue(work) = state
        .push
        .process_publish_phase1(&state.job.to_string(), crate::time::now_ms())
        .await
        .expect("phase 1")
    else {
        panic!("claim must continue");
    };
    work.job
}

fn attempt(status: &'static str) -> DispatchedAttempt {
    DispatchedAttempt {
        device_id: "device".to_string(),
        platform: PushDevicePlatform::Web,
        status,
        last_error: Some(status.to_string()),
        retry_after: None,
    }
}

async fn unknown(state: &State) {
    state
        .push
        .execute(
            "UPDATE push_publish_jobs SET attempt_count = 0 WHERE job_id = ?",
            crate::db_params![state.job.to_string()],
        )
        .await
        .expect("start retry budget");
    let job = claim(state).await;
    state
        .push
        .finalize_publish_job(
            &job,
            &[attempt(dispatch::ATTEMPT_STATUS_WEB_TRANSIENT)],
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
            crate::time::now_ms(),
        )
        .await
        .expect("lost provider response");
}

async fn assert_pending(state: &State) {
    let job = state
        .push
        .load_publish_job(&state.job.to_string())
        .await
        .expect("load")
        .expect("job");
    assert_eq!(
        job.status(),
        PUBLISH_JOB_STATUS_QUEUED,
        "a later known failure cannot resolve an earlier request"
    );
    assert!(job.uncertain_send);
    assert!(job.claim_token().is_empty());
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ? AND attempt_count > 0 AND next_retry_at_ms IS NOT NULL AND claimed_at_ms IS NULL AND published_at_ms IS NULL", crate::db_params![state.job.to_string()]).await, 1);
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![state.delivery.to_string()]).await, 2, "all canonical parents retain custody");
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM ingress_messages m WHERE retention_eligible_at IS NOT NULL AND EXISTS (SELECT 1 FROM ingress_effect_descendants d WHERE d.message_key = m.message_key AND d.descendant_key = ?)", crate::db_params![state.delivery.to_string()]).await, 0);
}

async fn assert_parked(state: &State) {
    let job = state
        .push
        .load_publish_job(&state.job.to_string())
        .await
        .expect("load")
        .expect("parked job");
    assert_eq!(job.status(), PUBLISH_JOB_STATUS_FAILED);
    assert!(job.uncertain_send);
    assert!(job.claim_token().is_empty());
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ? AND next_retry_at_ms IS NULL AND claimed_at_ms IS NULL AND published_at_ms IS NULL", crate::db_params![state.job.to_string()]).await, 1);
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![state.delivery.to_string()]).await, 2, "parked uncertainty retains every parent's custody");
    assert!(
        state
            .push
            .process_publish_job_with_retention_limit(
                &state.job.to_string(),
                MAX_DELIVERY_ATTEMPTS_PER_NODE
            )
            .await
            .expect("parked worker retry")
            .is_none(),
        "parked jobs must not send again"
    );
}

async fn sticky_uncertainty_survives_known_failures(fixture: IngressFixture) {
    for (index, status) in [
        dispatch::ATTEMPT_STATUS_WEB_BAD_REQUEST,
        dispatch::ATTEMPT_STATUS_WEB_PAYLOAD_TOO_LARGE,
        ATTEMPT_STATUS_WEB_INTERNAL_ERROR,
        apns_dispatch::ATTEMPT_STATUS_APNS_PROVIDER_AUTH,
        apns_dispatch::ATTEMPT_STATUS_APNS_TOPIC_MISMATCH,
    ]
    .into_iter()
    .enumerate()
    {
        let state = seed(&fixture, &format!("unknown-known-{index}")).await;
        unknown(&state).await;
        let job = claim(&state).await;
        state
            .push
            .finalize_publish_job(
                &job,
                &[attempt(status)],
                MAX_DELIVERY_ATTEMPTS_PER_NODE,
                crate::time::now_ms(),
            )
            .await
            .expect("known failure");
        assert_pending(&state).await;
    }
    // The durable flag can change after phase 1 captured its job snapshot.
    let state = seed(&fixture, "uncertainty-upgraded-after-claim").await;
    let job = claim(&state).await;
    assert!(!job.uncertain_send);
    state
        .push
        .execute(
            "UPDATE push_publish_jobs SET uncertain_send = 1, attempt_count = 50 WHERE job_id = ?",
            crate::db_params![state.job.to_string()],
        )
        .await
        .expect("outer repair records uncertainty");
    state
        .push
        .finalize_publish_job(
            &job,
            &[attempt(dispatch::ATTEMPT_STATUS_WEB_BAD_REQUEST)],
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
            crate::time::now_ms(),
        )
        .await
        .expect("fresh durable uncertainty");
    assert_parked(&state).await;
    fixture.close().await;
}

async fn automatic_disable_cannot_settle_earlier_unknown_send(fixture: IngressFixture) {
    let state = seed(&fixture, "unknown-before-auto-disable").await;
    unknown(&state).await;
    let other = state
        .push
        .enqueue_canonical_notification_publish_job(
            state.node.node(),
            &notification_item("registration-cleanup"),
            &owner(),
            &state.service,
            None,
            uuid::Uuid::new_v4(),
        )
        .await
        .expect("other accepted job");
    let Phase1Outcome::Continue(work) = state
        .push
        .process_publish_phase1(&other.job_id().to_string(), crate::time::now_ms())
        .await
        .expect("other claim")
    else {
        panic!("other job must be claimed");
    };
    state
        .push
        .finalize_publish_job(
            &work.job,
            &[attempt(dispatch::ATTEMPT_STATUS_WEB_GONE)],
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
            crate::time::now_ms(),
        )
        .await
        .expect("actual automatic registration disable");
    state
        .push
        .execute(
            "UPDATE push_publish_jobs SET next_retry_at_ms = NULL WHERE job_id = ?",
            crate::db_params![state.job.to_string()],
        )
        .await
        .expect("retry unknown job");
    let result = state
        .push
        .process_publish_job_with_retention_limit(
            &state.job.to_string(),
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
        )
        .await
        .expect("preflight refuses before provider send")
        .expect("attempted job");
    assert_eq!(result.attempted_devices(), 0);
    assert_pending(&state).await;
    fixture.close().await;
}

async fn parse_failure_cannot_fail_successor_claim(fixture: IngressFixture) {
    let state = seed(&fixture, "parse-successor-fence").await;
    let captured = claim(&state).await;
    let successor = uuid::Uuid::new_v4().to_string();
    state
        .push
        .execute(
            "UPDATE push_publish_jobs SET claim_token = ?, uncertain_send = 1 WHERE job_id = ?",
            crate::db_params![successor.clone(), state.job.to_string()],
        )
        .await
        .expect("successor lease");
    state
        .push
        .mark_publish_job_failed_after_phase1(
            &captured,
            "payload parse failed",
            crate::time::now_ms(),
        )
        .await
        .expect("stale parser result");
    let job = state
        .push
        .load_publish_job(captured.job_id())
        .await
        .expect("load")
        .expect("successor");
    assert_eq!(job.status(), PUBLISH_JOB_STATUS_IN_PROGRESS);
    assert_eq!(job.claim_token(), successor);
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![state.delivery.to_string()]).await, 2);
    fixture.close().await;
}

async fn known_failure_success_and_explicit_cancellation_controls(fixture: IngressFixture) {
    let known = seed(&fixture, "known-encoder-failure-control").await;
    let job = claim(&known).await;
    known
        .push
        .finalize_publish_job(
            &job,
            &[attempt(dispatch::ATTEMPT_STATUS_WEB_BAD_REQUEST)],
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
            crate::time::now_ms(),
        )
        .await
        .expect("known failure");
    assert_eq!(
        known
            .push
            .load_publish_job(job.job_id())
            .await
            .expect("load")
            .expect("known job")
            .status(),
        PUBLISH_JOB_STATUS_FAILED
    );
    assert_eq!(scalar_i64(&known.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![known.delivery.to_string()]).await, 0);
    let delivered = seed(&fixture, "later-delivery-control").await;
    unknown(&delivered).await;
    let job = claim(&delivered).await;
    delivered
        .push
        .finalize_publish_job(
            &job,
            &[attempt(dispatch::ATTEMPT_STATUS_WEB_DELIVERED)],
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
            crate::time::now_ms(),
        )
        .await
        .expect("later definitive success");
    assert_eq!(
        delivered
            .push
            .load_publish_job(job.job_id())
            .await
            .expect("load")
            .expect("delivered job")
            .status(),
        PUBLISH_JOB_STATUS_PUBLISHED
    );
    assert_eq!(scalar_i64(&delivered.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![delivered.delivery.to_string()]).await, 0);
    // Another acceptance of the same wire item cannot resolve this job's send.
    let foreign = seed(&fixture, "foreign-delivery-control").await;
    unknown(&foreign).await;
    let other = foreign
        .push
        .enqueue_canonical_notification_publish_job(
            foreign.node.node(),
            &notification_item("foreign-delivery-control"),
            &owner(),
            &foreign.service,
            None,
            uuid::Uuid::new_v4(),
        )
        .await
        .expect("distinct acceptance of same item");
    assert_ne!(other.job_id(), foreign.job);
    let Phase1Outcome::Continue(work) = foreign
        .push
        .process_publish_phase1(&other.job_id().to_string(), crate::time::now_ms())
        .await
        .expect("foreign claim")
    else {
        panic!("foreign job must be claimed");
    };
    foreign
        .push
        .finalize_publish_job(
            &work.job,
            &[attempt(dispatch::ATTEMPT_STATUS_WEB_DELIVERED)],
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
            crate::time::now_ms(),
        )
        .await
        .expect("foreign definitive success");
    assert_eq!(scalar_i64(&foreign.push, "SELECT COUNT(*) FROM push_delivery_attempts WHERE publish_job_id = ? AND status = 'web-delivered'", crate::db_params![other.job_id().to_string()]).await, 1);
    assert_eq!(scalar_i64(&foreign.push, "SELECT COUNT(*) FROM push_delivery_attempts WHERE publish_job_id = ? AND status = 'web-delivered'", crate::db_params![foreign.job.to_string()]).await, 0);
    let job = claim(&foreign).await;
    foreign
        .push
        .finalize_publish_job(
            &job,
            &[attempt(dispatch::ATTEMPT_STATUS_WEB_PAYLOAD_TOO_LARGE)],
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
            crate::time::now_ms(),
        )
        .await
        .expect("original known failure");
    assert_pending(&foreign).await;
    // A confirmed acceptance in the current retry also completes this job.
    let mixed = seed(&fixture, "current-mixed-delivery-control").await;
    mixed
        .push
        .upsert_device(
            &owner(),
            PushDeviceRegistration::new(
                "permanent-device",
                mixed.node.node(),
                PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("second device");
    unknown(&mixed).await;
    let job = claim(&mixed).await;
    let mut permanent = attempt(dispatch::ATTEMPT_STATUS_WEB_PAYLOAD_TOO_LARGE);
    permanent.device_id = "permanent-device".to_string();
    mixed
        .push
        .finalize_publish_job(
            &job,
            &[attempt(dispatch::ATTEMPT_STATUS_WEB_DELIVERED), permanent],
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
            crate::time::now_ms(),
        )
        .await
        .expect("current mixed definitive success");
    assert_eq!(
        mixed
            .push
            .load_publish_job(job.job_id())
            .await
            .expect("load")
            .expect("mixed job")
            .status(),
        PUBLISH_JOB_STATUS_PUBLISHED
    );
    assert_eq!(scalar_i64(&mixed.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![mixed.delivery.to_string()]).await, 0);
    let canceled = seed(&fixture, "explicit-cancellation-control").await;
    unknown(&canceled).await;
    canceled
        .push
        .disable_nodes_for_owner(&owner(), Some(canceled.node.node()))
        .await
        .expect("explicit user revocation");
    assert_eq!(
        canceled
            .push
            .load_publish_job(&canceled.job.to_string())
            .await
            .expect("load")
            .expect("canceled job")
            .status(),
        PUBLISH_JOB_STATUS_FAILED
    );
    assert_eq!(scalar_i64(&canceled.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![canceled.delivery.to_string()]).await, 0);
    fixture.close().await;
}

/// Exercise the persisted worker seam through the entire budget. Receipt of a
/// generic server error is deliberately distinct from a documented refusal.
async fn transient_attempt_budget_bounds_all_failure_classes(fixture: IngressFixture) {
    for prior_unknown in [false, true] {
        for status in [
            dispatch::ATTEMPT_STATUS_WEB_SERVER_ERROR,
            apns_dispatch::ATTEMPT_STATUS_APNS_SERVER_ERROR,
            fcm_dispatch::ATTEMPT_STATUS_FCM_SERVER_ERROR,
            dispatch::ATTEMPT_STATUS_WEB_RATE_LIMITED,
            apns_dispatch::ATTEMPT_STATUS_APNS_RATE_LIMITED,
            fcm_dispatch::ATTEMPT_STATUS_FCM_RATE_LIMITED,
            apns_dispatch::ATTEMPT_STATUS_APNS_PROVIDER_REFUSED,
            fcm_dispatch::ATTEMPT_STATUS_FCM_PROVIDER_REFUSED,
            ATTEMPT_STATUS_WEB_NOT_CONFIGURED,
            fcm_dispatch::ATTEMPT_STATUS_FCM_TOKEN_TRANSIENT,
            dispatch::ATTEMPT_STATUS_WEB_TRANSIENT,
            apns_dispatch::ATTEMPT_STATUS_APNS_TRANSIENT,
            fcm_dispatch::ATTEMPT_STATUS_FCM_TRANSIENT,
        ] {
            let state = seed(&fixture, &format!("budget-{prior_unknown}-{status}")).await;
            if prior_unknown {
                state
                    .push
                    .execute(
                        "UPDATE push_publish_jobs SET uncertain_send = 1 WHERE job_id = ?",
                        crate::db_params![state.job.to_string()],
                    )
                    .await
                    .expect("earlier possible send");
            }
            let uncertain = prior_unknown
                || attempt_failure_class(status) == PushFailureClass::UnknownAfterSend;
            for completed in 1..=PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS {
                let job = claim(&state).await;
                state
                    .push
                    .finalize_publish_job(
                        &job,
                        &[attempt(status)],
                        MAX_DELIVERY_ATTEMPTS_PER_NODE,
                        crate::time::now_ms(),
                    )
                    .await
                    .expect("persist attempt");
                let current = state
                    .push
                    .load_publish_job(&state.job.to_string())
                    .await
                    .expect("load")
                    .expect("job");
                assert_eq!(
                    current.uncertain_send, uncertain,
                    "{status} prior={prior_unknown}"
                );
                if completed < PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS {
                    assert_eq!(current.status(), PUBLISH_JOB_STATUS_QUEUED);
                } else {
                    assert_eq!(current.status(), PUBLISH_JOB_STATUS_FAILED);
                }
            }
            assert_eq!(
                scalar_i64(
                    &state.push,
                    "SELECT attempt_count FROM push_publish_jobs WHERE job_id = ?",
                    crate::db_params![state.job.to_string()]
                )
                .await,
                PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS
            );
            assert_eq!(
                scalar_i64(
                    &state.push,
                    "SELECT COUNT(*) FROM push_delivery_attempts WHERE publish_job_id = ?",
                    crate::db_params![state.job.to_string()]
                )
                .await,
                PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS
            );
            if uncertain {
                assert_parked(&state).await;
            } else {
                assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![state.delivery.to_string()]).await, 0, "proven failure releases every parent");
            }
            assert!(state
                .push
                .process_publish_job_with_retention_limit(
                    &state.job.to_string(),
                    MAX_DELIVERY_ATTEMPTS_PER_NODE
                )
                .await
                .expect("exhausted job")
                .is_none());
            assert_eq!(
                scalar_i64(
                    &state.push,
                    "SELECT attempt_count FROM push_publish_jobs WHERE job_id = ?",
                    crate::db_params![state.job.to_string()]
                )
                .await,
                PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS,
                "exhausted work cannot start another attempt"
            );
            // Release the fixture's active-node slot without changing this
            // acceptance's custody or applying an explicit user disposition.
            state
                .push
                .execute(
                    "UPDATE push_nodes SET status = 'disabled' WHERE node = ?",
                    crate::db_params![state.node.node()],
                )
                .await
                .expect("release fixture node slot");
        }
    }
    fixture.close().await;
}

async fn no_device_attempt_budget_is_bounded(fixture: IngressFixture) {
    for uncertain in [false, true] {
        let state = seed(&fixture, &format!("no-devices-{uncertain}")).await;
        state
            .push
            .execute(
                "UPDATE push_devices SET status = 'disabled' WHERE node = ?",
                crate::db_params![state.node.node()],
            )
            .await
            .expect("no active devices");
        state.push.execute("UPDATE push_publish_jobs SET uncertain_send = ?, attempt_count = ? WHERE job_id = ?", crate::db_params![i64::from(uncertain), PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS - 1, state.job.to_string()]).await.expect("last budget attempt");
        assert!(state
            .push
            .process_publish_job_with_retention_limit(
                &state.job.to_string(),
                MAX_DELIVERY_ATTEMPTS_PER_NODE
            )
            .await
            .expect("no device attempt")
            .is_some());
        let current = state
            .push
            .load_publish_job(&state.job.to_string())
            .await
            .expect("load")
            .expect("job");
        assert_eq!(current.status(), PUBLISH_JOB_STATUS_FAILED);
        assert_eq!(current.uncertain_send, uncertain);
        if uncertain {
            assert_parked(&state).await;
        } else {
            assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![state.delivery.to_string()]).await, 0);
        }
        assert!(state
            .push
            .process_publish_job_with_retention_limit(
                &state.job.to_string(),
                MAX_DELIVERY_ATTEMPTS_PER_NODE
            )
            .await
            .expect("exhausted no device")
            .is_none());
    }
    fixture.close().await;
}

async fn explicit_revocation_settles_capped_known_failure_custody(fixture: IngressFixture) {
    let state = seed(&fixture, "known-cap-awaiting-custody-repair").await;
    state
        .push
        .execute(
            "UPDATE push_publish_jobs SET attempt_count = ? WHERE job_id = ?",
            crate::db_params![
                PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS - 1,
                state.job.to_string()
            ],
        )
        .await
        .expect("last pre-dispatch attempt");
    if fixture.db.driver() == crate::db::DatabaseDriver::Postgres {
        // Exercise the real atomic fallback: the queued job reaches its cap
        // while Foundation holds its canonical parents, so repair can stop
        // dispatch but cannot settle custody in that transaction.
        let mut held = fixture.db.begin().await.expect("hold canonical parents");
        let mut rows = held.query("SELECT message_key FROM ingress_messages WHERE message_key IN (SELECT message_key FROM ingress_effect_descendants WHERE descendant_key = ?) FOR UPDATE", crate::db_params![state.delivery.to_string()]).await.expect("parent locks");
        while rows.next().await.expect("locked parent").is_some() {}
        drop(rows);
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            state.push.record_publish_job_failure_by_id(
                &state.job.to_string(),
                "contended pre-dispatch failure",
            ),
        )
        .await
        .expect("repair must not wait for canonical locks")
        .expect("bounded repair");
        held.commit().await.expect("release parents");
    } else {
        // SQLite's database writer lock cannot represent this independent
        // parent-row contention; seed the same durable atomic-repair state.
        state.push.execute("UPDATE push_publish_jobs SET status = 'failed', attempt_count = ?, next_retry_at_ms = NULL, last_error = 'contended pre-dispatch failure' WHERE job_id = ?", crate::db_params![PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS, state.job.to_string()]).await.expect("seed capped known failure awaiting custody repair");
    }
    let capped = state
        .push
        .load_publish_job(&state.job.to_string())
        .await
        .expect("load")
        .expect("capped");
    assert_eq!(capped.status(), PUBLISH_JOB_STATUS_FAILED);
    assert!(!capped.uncertain_send, "no request reached a provider");
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ? AND terminal_disposition IS NULL", crate::db_params![state.job.to_string()]).await, 1);
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![state.delivery.to_string()]).await, 2, "capped repair retained the contended parents");
    state
        .push
        .disable_nodes_for_owner(&owner(), Some(state.node.node()))
        .await
        .expect("explicit registration revocation");
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ? AND status = 'failed' AND uncertain_send = 0 AND terminal_disposition = 'registration-revoked'", crate::db_params![state.job.to_string()]).await, 1, "explicit revocation records its terminal disposition on known failures too");
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![state.delivery.to_string()]).await, 0);
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM ingress_messages m WHERE retention_eligible_at IS NOT NULL AND EXISTS (SELECT 1 FROM ingress_effect_descendants d WHERE d.message_key = m.message_key AND d.descendant_key = ?)", crate::db_params![state.delivery.to_string()]).await, 2, "both canonical parents are eligible after explicit disposition");
    assert!(state
        .push
        .delivery_attempts_for_node(state.node.node())
        .await
        .expect("provider attempts")
        .is_empty());
    fixture.close().await;
}

macro_rules! backend_tests {
    ($sqlite:ident, $postgres:ident, $helper:ident, $fixture:literal) => {
        #[tokio::test]
        async fn $sqlite() {
            $helper(IngressFixture::sqlite().await).await;
        }
        #[tokio::test]
        async fn $postgres() {
            if let Some(fixture) = IngressFixture::postgres($fixture).await {
                $helper(fixture).await;
            }
        }
    };
}
backend_tests!(
    sqlite_sticky_uncertainty_survives_known_failures,
    postgres_sticky_uncertainty_survives_known_failures,
    sticky_uncertainty_survives_known_failures,
    "pushsticky"
);
backend_tests!(
    sqlite_automatic_disable_cannot_settle_earlier_unknown_send,
    postgres_automatic_disable_cannot_settle_earlier_unknown_send,
    automatic_disable_cannot_settle_earlier_unknown_send,
    "pushauto"
);
backend_tests!(
    sqlite_parse_failure_cannot_fail_successor_claim,
    postgres_parse_failure_cannot_fail_successor_claim,
    parse_failure_cannot_fail_successor_claim,
    "pushparse"
);
backend_tests!(
    sqlite_known_failure_success_and_explicit_cancellation_controls,
    postgres_known_failure_success_and_explicit_cancellation_controls,
    known_failure_success_and_explicit_cancellation_controls,
    "pushctrl"
);

backend_tests!(
    sqlite_transient_attempt_budget_bounds_all_failure_classes,
    postgres_transient_attempt_budget_bounds_all_failure_classes,
    transient_attempt_budget_bounds_all_failure_classes,
    "pushbudget"
);
backend_tests!(
    sqlite_no_device_attempt_budget_is_bounded,
    postgres_no_device_attempt_budget_is_bounded,
    no_device_attempt_budget_is_bounded,
    "pushnodevice"
);

backend_tests!(
    sqlite_explicit_revocation_settles_capped_known_failure_custody,
    postgres_explicit_revocation_settles_capped_known_failure_custody,
    explicit_revocation_settles_capped_known_failure_custody,
    "pushknownrevoke"
);
