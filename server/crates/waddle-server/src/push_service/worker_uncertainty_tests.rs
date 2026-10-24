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
    crate::push_registrations::DatabasePushRegistrationStore::try_new(fixture.db.clone())
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
    // Fixture statuses represent already-classified provider results; production
    // scheduling carries this evidence directly from typed outcome mappers.
    let retry = match status {
        dispatch::ATTEMPT_STATUS_WEB_CONNECT_FAILED
        | apns_dispatch::ATTEMPT_STATUS_APNS_CONNECT_FAILED
        | fcm_dispatch::ATTEMPT_STATUS_FCM_CONNECT_FAILED
        | ATTEMPT_STATUS_WEB_NOT_CONFIGURED
        | fcm_dispatch::ATTEMPT_STATUS_FCM_TOKEN_TRANSIENT => {
            Some(dispatch::RetryMetadata::PRE_SEND)
        }
        dispatch::ATTEMPT_STATUS_WEB_SERVER_ERROR
        | apns_dispatch::ATTEMPT_STATUS_APNS_SERVER_ERROR
        | fcm_dispatch::ATTEMPT_STATUS_FCM_SERVER_ERROR => {
            Some(dispatch::RetryMetadata::for_transport_failure(
                &waddle_xmpp::push::types::TransientFailure::ServerError { status: 503 },
            ))
        }
        dispatch::ATTEMPT_STATUS_WEB_TRANSIENT
        | apns_dispatch::ATTEMPT_STATUS_APNS_TRANSIENT
        | fcm_dispatch::ATTEMPT_STATUS_FCM_TRANSIENT => {
            Some(dispatch::RetryMetadata::for_transport_failure(
                &waddle_xmpp::push::types::TransientFailure::Timeout,
            ))
        }
        dispatch::ATTEMPT_STATUS_WEB_RATE_LIMITED
        | apns_dispatch::ATTEMPT_STATUS_APNS_RATE_LIMITED
        | fcm_dispatch::ATTEMPT_STATUS_FCM_RATE_LIMITED => Some(dispatch::RetryMetadata::refused(
            PushRetryReason::RateLimited,
        )),
        apns_dispatch::ATTEMPT_STATUS_APNS_PROVIDER_REFUSED
        | fcm_dispatch::ATTEMPT_STATUS_FCM_PROVIDER_REFUSED => Some(
            dispatch::RetryMetadata::refused(PushRetryReason::ProviderServerError),
        ),
        dispatch::ATTEMPT_STATUS_WEB_CLOCK_SKEW => {
            Some(dispatch::RetryMetadata::refused(PushRetryReason::PreSend))
        }
        _ => None,
    };
    DispatchedAttempt {
        device_id: "device".to_string(),
        platform: PushDevicePlatform::Web,
        status,
        last_error: Some(status.to_string()),
        retry_after: None,
        retry,
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

async fn known_failure_after_uncertainty_emits_retry_and_parked_dispositions(
    fixture: IngressFixture,
) {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    for exhausted in [false, true] {
        for status in [
            dispatch::ATTEMPT_STATUS_WEB_BAD_REQUEST,
            dispatch::ATTEMPT_STATUS_WEB_PAYLOAD_TOO_LARGE,
            ATTEMPT_STATUS_WEB_INTERNAL_ERROR,
            apns_dispatch::ATTEMPT_STATUS_APNS_PROVIDER_AUTH,
            apns_dispatch::ATTEMPT_STATUS_APNS_TOPIC_MISMATCH,
        ] {
            let state = seed(&fixture, &format!("known-metrics-{exhausted}-{status}")).await;
            unknown(&state).await;
            let job = claim(&state).await;
            if exhausted {
                // The transition's evidence must come from the durable row,
                // not the attempt count captured by phase 1.
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
                    .expect("advance durable retry budget after claim");
            }
            let disposition = if exhausted {
                PushDisposition::Parked
            } else {
                PushDisposition::Retry
            };
            let counts = || {
                [
                    metrics.counter_sum(
                        "xmpp.push.failure_reason",
                        &[("reason", "unknown_after_send")],
                    ),
                    metrics.counter_sum(
                        "xmpp.push.failure_class",
                        &[("failure_class", "unknown_after_send")],
                    ),
                    metrics.counter_sum(
                        "xmpp.push.disposition",
                        &[("disposition", disposition.as_str())],
                    ),
                ]
                .map(|count| count.unwrap_or(0))
            };
            let before = counts();
            let failure = attempt(status);
            assert!(failure.retry.is_none(), "exercise retained uncertainty");
            state
                .push
                .finalize_publish_job(
                    &job,
                    std::slice::from_ref(&failure),
                    MAX_DELIVERY_ATTEMPTS_PER_NODE,
                    crate::time::now_ms(),
                )
                .await
                .expect("known failure after unknown send");
            let after = before.map(|count| count + 1);
            assert_eq!(counts(), after, "{status}, exhausted={exhausted}");
            if exhausted {
                assert_parked(&state).await;
            } else {
                assert_pending(&state).await;
            }
            state
                .push
                .finalize_publish_job(
                    &job,
                    &[failure],
                    MAX_DELIVERY_ATTEMPTS_PER_NODE,
                    crate::time::now_ms(),
                )
                .await
                .expect("stale callback loses its cleared claim");
            assert_eq!(counts(), after, "a stale fence cannot emit a transition");
            assert_eq!(
                scalar_i64(
                    &state.push,
                    "SELECT COUNT(*) FROM push_delivery_attempts WHERE publish_job_id = ?",
                    crate::db_params![state.job.to_string()]
                )
                .await,
                2,
                "a stale callback cannot append another provider attempt"
            );
        }
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_prior_uncertainty_disposition_is_not_emitted_before_commit() {
    let metrics = waddle_xmpp::telemetry::test_support::acquire().await;
    let fixture = IngressFixture::sqlite().await;
    let state = seed(&fixture, "rolled-back-known-failure-disposition").await;
    unknown(&state).await;
    let job = claim(&state).await;
    let counts = || {
        [
            metrics.counter_sum(
                "xmpp.push.failure_reason",
                &[("reason", "unknown_after_send")],
            ),
            metrics.counter_sum(
                "xmpp.push.failure_class",
                &[("failure_class", "unknown_after_send")],
            ),
            metrics.counter_sum("xmpp.push.disposition", &[("disposition", "retry")]),
        ]
        .map(|count| count.unwrap_or(0))
    };
    let before = counts();
    state
        .push
        .execute(
            "CREATE TRIGGER fail_provider_attempt_prune BEFORE DELETE ON push_delivery_attempts BEGIN SELECT RAISE(ABORT, 'prune denied'); END",
            (),
        )
        .await
        .expect("inject a failure after the known-failure transition");
    assert!(state
        .push
        .finalize_publish_job(
            &job,
            &[attempt(dispatch::ATTEMPT_STATUS_WEB_BAD_REQUEST)],
            0,
            crate::time::now_ms(),
        )
        .await
        .is_err());
    assert_eq!(counts(), before, "rolled-back transitions emit nothing");
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ? AND status = 'in-progress' AND claim_token = ? AND attempt_count = 1 AND uncertain_send = 1", crate::db_params![state.job.to_string(), job.claim_token()]).await, 1, "the failed finalization preserves the current claim and durable retry history");
    assert_eq!(
        scalar_i64(
            &state.push,
            "SELECT COUNT(*) FROM push_delivery_attempts WHERE publish_job_id = ?",
            crate::db_params![state.job.to_string()]
        )
        .await,
        1,
        "the new attempt rolled back with the transition"
    );
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![state.delivery.to_string()]).await, 2);
    state
        .push
        .execute("DROP TRIGGER fail_provider_attempt_prune", ())
        .await
        .expect("restore pruning");
    state
        .push
        .finalize_publish_job(
            &job,
            &[attempt(dispatch::ATTEMPT_STATUS_WEB_BAD_REQUEST)],
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
            crate::time::now_ms(),
        )
        .await
        .expect("commit the same claim exactly once");
    assert_eq!(counts(), before.map(|count| count + 1));
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

async fn prior_delivery_completes_after_all_devices_become_inactive(fixture: IngressFixture) {
    let state = seed(&fixture, "confirmed-before-all-devices-inactive").await;
    state
        .push
        .upsert_device(
            &owner(),
            PushDeviceRegistration::new(
                "uncertain-device",
                state.node.node(),
                PushDevicePlatform::Web,
                "test",
            ),
        )
        .await
        .expect("second device");
    let job = claim(&state).await;
    let mut uncertain = attempt(dispatch::ATTEMPT_STATUS_WEB_TRANSIENT);
    uncertain.device_id = "uncertain-device".to_string();
    state
        .push
        .finalize_publish_job(
            &job,
            &[attempt(dispatch::ATTEMPT_STATUS_WEB_DELIVERED), uncertain],
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
            crate::time::now_ms(),
        )
        .await
        .expect("one confirmed delivery and one unknown outcome");
    assert_pending(&state).await;
    state
        .push
        .execute(
            "UPDATE push_devices SET status = 'disabled' WHERE node = ?",
            crate::db_params![state.node.node()],
        )
        .await
        .expect("all devices become inactive");
    state
        .push
        .execute(
            "UPDATE push_publish_jobs SET next_retry_at_ms = NULL WHERE job_id = ?",
            crate::db_params![state.job.to_string()],
        )
        .await
        .expect("retry now");
    let result = state
        .push
        .process_publish_job_with_retention_limit(
            &state.job.to_string(),
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
        )
        .await
        .expect("complete from durable acceptance proof")
        .expect("completed job");
    assert_eq!(result.attempted_devices(), 0);
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ? AND status = 'published' AND attempt_count = 2 AND uncertain_send = 1 AND last_error IS NULL AND next_retry_at_ms IS NULL AND published_at_ms IS NOT NULL AND claim_token IS NULL AND claimed_at_ms IS NULL", crate::db_params![state.job.to_string()]).await, 1, "completion keeps the historical uncertainty while recording authoritative delivery");
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![state.delivery.to_string()]).await, 0);
    assert_eq!(
        scalar_i64(
            &state.push,
            "SELECT COUNT(*) FROM push_delivery_attempts WHERE publish_job_id = ?",
            crate::db_params![state.job.to_string()]
        )
        .await,
        2
    );
    assert!(state
        .push
        .process_publish_job_with_retention_limit(
            &state.job.to_string(),
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
        )
        .await
        .expect("terminal job stays complete")
        .is_none());
    assert_eq!(
        scalar_i64(
            &state.push,
            "SELECT COUNT(*) FROM push_delivery_attempts WHERE publish_job_id = ?",
            crate::db_params![state.job.to_string()]
        )
        .await,
        2,
        "completion never replays provider requests"
    );

    // A delivery for another acceptance of the exact same wire item is not
    // authoritative evidence for the original acceptance's unknown send.
    let foreign = seed(&fixture, "foreign-delivery-before-inactive").await;
    unknown(&foreign).await;
    let other = foreign
        .push
        .enqueue_canonical_notification_publish_job(
            foreign.node.node(),
            &notification_item("foreign-delivery-before-inactive"),
            &owner(),
            &foreign.service,
            None,
            uuid::Uuid::new_v4(),
        )
        .await
        .expect("distinct acceptance");
    let Phase1Outcome::Continue(work) = foreign
        .push
        .process_publish_phase1(&other.job_id().to_string(), crate::time::now_ms())
        .await
        .expect("claim distinct acceptance")
    else {
        panic!("distinct acceptance must be claimed");
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
        .expect("other acceptance delivered");
    foreign
        .push
        .execute(
            "UPDATE push_devices SET status = 'disabled' WHERE node = ?",
            crate::db_params![foreign.node.node()],
        )
        .await
        .expect("all devices inactive");
    foreign
        .push
        .execute(
            "UPDATE push_publish_jobs SET next_retry_at_ms = NULL WHERE job_id = ?",
            crate::db_params![foreign.job.to_string()],
        )
        .await
        .expect("retry original acceptance");
    let result = foreign
        .push
        .process_publish_job_with_retention_limit(
            &foreign.job.to_string(),
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
        )
        .await
        .expect("no current devices")
        .expect("original remains pending");
    assert_eq!(result.attempted_devices(), 0);
    assert_pending(&foreign).await;
    assert_eq!(
        scalar_i64(
            &foreign.push,
            "SELECT COUNT(*) FROM push_delivery_attempts WHERE publish_job_id = ?",
            crate::db_params![foreign.job.to_string()]
        )
        .await,
        1,
        "the empty-node retry cannot send or copy another acceptance's delivery"
    );
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
            dispatch::ATTEMPT_STATUS_WEB_CONNECT_FAILED,
            apns_dispatch::ATTEMPT_STATUS_APNS_CONNECT_FAILED,
            fcm_dispatch::ATTEMPT_STATUS_FCM_CONNECT_FAILED,
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
                || attempt(status)
                    .retry
                    .is_some_and(|retry| retry.failure_class == PushFailureClass::UnknownAfterSend);
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

async fn scheduling_uses_typed_retry_evidence(fixture: IngressFixture) {
    let state = seed(&fixture, "typed-retry-evidence").await;
    let job = claim(&state).await;
    let mut classified = attempt("opaque-provider-diagnostic");
    classified.retry = Some(dispatch::RetryMetadata::PRE_SEND);
    state
        .push
        .finalize_publish_job(
            &job,
            &[classified],
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
            crate::time::now_ms(),
        )
        .await
        .expect("typed retry");
    let current = state
        .push
        .load_publish_job(&state.job.to_string())
        .await
        .expect("load")
        .expect("job");
    assert_eq!(
        current.status(),
        PUBLISH_JOB_STATUS_QUEUED,
        "a diagnostic string cannot suppress a typed retry"
    );
    assert!(!current.uncertain_send);
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![state.delivery.to_string()]).await, 2);
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

async fn direct_stale_claim_recovery_respects_persisted_exponential_backoff(
    fixture: IngressFixture,
) {
    let state = seed(&fixture, "direct-expired-claim-backoff").await;
    let _claim = claim(&state).await;
    state
        .push
        .execute(
            "UPDATE push_publish_jobs SET claimed_at_ms = 1, attempt_count = 2 WHERE job_id = ?",
            crate::db_params![state.job.to_string()],
        )
        .await
        .expect("expired third attempt");
    let started = crate::time::now_ms();
    assert!(
        state
            .push
            .process_publish_job_with_retention_limit(
                &state.job.to_string(),
                MAX_DELIVERY_ATTEMPTS_PER_NODE
            )
            .await
            .expect("direct recovery")
            .is_none(),
        "recovering a lease does not bypass provider backoff"
    );
    let finished = crate::time::now_ms();
    let mut rows = state.push.query("SELECT status, uncertain_send, attempt_count, next_retry_at_ms, claim_token, claimed_at_ms FROM push_publish_jobs WHERE job_id = ?", crate::db_params![state.job.to_string()]).await.expect("recovered state");
    let row = rows.next().await.expect("row").expect("work");
    assert_eq!(
        row.get::<String>(0).expect("status"),
        PUBLISH_JOB_STATUS_QUEUED
    );
    assert_eq!(row.get::<i64>(1).expect("uncertainty"), 1);
    assert_eq!(row.get::<i64>(2).expect("count"), 3);
    let deadline = row.get::<i64>(3).expect("persisted retry deadline");
    // Failed attempt three starts at 60s * 2^2, with ±25% jitter.
    assert!((started + 180_000..=finished + 300_000).contains(&deadline));
    assert_eq!(row.get::<Option<String>>(4).expect("claim token"), None);
    assert_eq!(row.get::<Option<i64>>(5).expect("claim time"), None);
    drop(rows);
    assert!(state
        .push
        .process_publish_job_with_retention_limit(
            &state.job.to_string(),
            MAX_DELIVERY_ATTEMPTS_PER_NODE
        )
        .await
        .expect("direct call before retry")
        .is_none());
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ? AND attempt_count = 3 AND next_retry_at_ms = ?", crate::db_params![state.job.to_string(), deadline]).await, 1, "repeated direct calls preserve the existing backoff");
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![state.delivery.to_string()]).await, 2);
    assert!(state
        .push
        .delivery_attempts_for_node(state.node.node())
        .await
        .expect("no provider attempts")
        .is_empty());
    fixture.close().await;
}

async fn seed_failed_custody(state: &State, uncertain: bool, retry_probe: i64) {
    state.push.execute("UPDATE push_publish_jobs SET status = 'failed', uncertain_send = ?, attempt_count = ?, last_error = 'preserve failure diagnostic', updated_at_ms = 123, next_retry_at_ms = ? WHERE job_id = ?", crate::db_params![i64::from(uncertain), PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS, retry_probe, state.job.to_string()]).await.expect("failed work awaiting custody repair");
}

async fn assert_failure_history_preserved(state: &State, uncertain: bool) {
    assert_eq!(scalar_i64(&state.push, "SELECT COUNT(*) FROM push_publish_jobs WHERE job_id = ? AND status = 'failed' AND uncertain_send = ? AND attempt_count = ? AND updated_at_ms = 123 AND last_error = 'preserve failure diagnostic' AND terminal_disposition IS NULL", crate::db_params![state.job.to_string(), i64::from(uncertain), PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS]).await, 1, "custody repair preserves the original failure history");
    assert!(state
        .push
        .delivery_attempts_for_node(state.node.node())
        .await
        .expect("provider attempts")
        .is_empty());
}

async fn known_failure_custody_reconciliation_preserves_uncertain_and_history(
    fixture: IngressFixture,
) {
    let known = seed(&fixture, "known-failure-custody-repair").await;
    let uncertain = seed(&fixture, "unknown-failure-retains-custody").await;
    seed_failed_custody(&known, false, 0).await;
    seed_failed_custody(&uncertain, true, 0).await;
    assert_eq!(
        known
            .push
            .reconcile_failed_publish_job_custody(16)
            .await
            .expect("repair known custody"),
        1
    );
    assert_eq!(scalar_i64(&known.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![known.delivery.to_string()]).await, 0);
    assert_eq!(scalar_i64(&uncertain.push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![uncertain.delivery.to_string()]).await, 2, "uncertain acceptance never gains automatic terminal proof");
    assert_eq!(scalar_i64(&known.push, "SELECT COUNT(*) FROM ingress_messages m WHERE retention_eligible_at IS NOT NULL AND EXISTS (SELECT 1 FROM ingress_effect_descendants d WHERE d.message_key = m.message_key AND d.descendant_key = ?)", crate::db_params![known.delivery.to_string()]).await, 2);
    assert_eq!(scalar_i64(&uncertain.push, "SELECT COUNT(*) FROM ingress_messages m WHERE retention_eligible_at IS NOT NULL AND EXISTS (SELECT 1 FROM ingress_effect_descendants d WHERE d.message_key = m.message_key AND d.descendant_key = ?)", crate::db_params![uncertain.delivery.to_string()]).await, 0);
    assert_failure_history_preserved(&known, false).await;
    assert_failure_history_preserved(&uncertain, true).await;
    assert_eq!(
        known
            .push
            .reconcile_failed_publish_job_custody(16)
            .await
            .expect("idempotent repair"),
        0
    );
    fixture.close().await;
}

#[tokio::test]
async fn postgres_contended_failed_custody_rotates_after_fresh_provider_work() {
    use crate::push_service::test_support::{
        fcm_registration, with_fake_fcm, FakeFcmSender, FCM_TEST_APP_ID,
    };
    let Some(fixture) = IngressFixture::postgres("pushcustodyrotate").await else {
        return;
    };
    let blocked = seed(&fixture, "contended-known-custody").await;
    let available = seed(&fixture, "available-known-custody").await;
    seed_failed_custody(&blocked, false, 0).await;
    seed_failed_custody(&available, false, 1).await;
    let sender = FakeFcmSender::default();
    let push = with_fake_fcm(blocked.push.clone(), sender.clone());
    let fresh = push
        .ensure_node(&owner(), FCM_TEST_APP_ID)
        .await
        .expect("fresh node");
    push.upsert_device(&owner(), fcm_registration("fresh-phone", fresh.node()))
        .await
        .expect("fresh device");
    push.enqueue_notification_publish_job_from_user_server(
        fresh.node(),
        &notification_item("fresh-before-custody-repair"),
        &owner(),
    )
    .await
    .expect("fresh work");
    let mut held = fixture.db.begin().await.expect("hold canonical parents");
    let mut rows = held.query("SELECT message_key FROM ingress_messages WHERE message_key IN (SELECT message_key FROM ingress_effect_descendants WHERE descendant_key = ?) FOR UPDATE", crate::db_params![blocked.delivery.to_string()]).await.expect("parent row locks");
    while rows.next().await.expect("locked parent").is_some() {}
    drop(rows);
    let results = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        push.drain_queued_notification_publish_jobs(1),
    )
    .await
    .expect("contended custody must not stall fresh work")
    .expect("fresh batch");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].item_id(), "fresh-before-custody-repair");
    assert_eq!(sender.sent(), 1);
    assert_eq!(scalar_i64(&push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![blocked.delivery.to_string()]).await, 2);
    assert_eq!(scalar_i64(&push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![available.delivery.to_string()]).await, 2, "the bounded sweep first tried the contended row");
    assert!(tokio::time::timeout(
        std::time::Duration::from_secs(2),
        push.drain_queued_notification_publish_jobs(1)
    )
    .await
    .expect("sweep rotates past contended ancestry")
    .expect("next batch")
    .is_empty());
    assert_eq!(scalar_i64(&push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![available.delivery.to_string()]).await, 0, "next bounded batch reaches another failed acceptance");
    assert_eq!(scalar_i64(&push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![blocked.delivery.to_string()]).await, 2);
    held.commit().await.expect("release canonical parents");
    assert!(push
        .drain_queued_notification_publish_jobs(1)
        .await
        .expect("repair after release")
        .is_empty());
    assert_eq!(scalar_i64(&push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![blocked.delivery.to_string()]).await, 0);
    assert_eq!(
        sender.sent(),
        1,
        "custody sweeps never replay provider requests"
    );
    assert_failure_history_preserved(&blocked, false).await;
    assert_failure_history_preserved(&available, false).await;
    fixture.close().await;
}

#[tokio::test]
async fn postgres_locked_failed_provider_row_does_not_block_fresh_work_or_custody_sweep() {
    use crate::push_service::test_support::{
        fcm_registration, with_fake_fcm, FakeFcmSender, FCM_TEST_APP_ID,
    };
    let Some(fixture) = IngressFixture::postgres("pushlockedchild").await else {
        return;
    };
    let blocked = seed(&fixture, "locked-failed-provider-row").await;
    seed_failed_custody(&blocked, false, 0).await;
    let sender = FakeFcmSender::default();
    let push = with_fake_fcm(blocked.push.clone(), sender.clone());
    let fresh = push
        .ensure_node(&owner(), FCM_TEST_APP_ID)
        .await
        .expect("fresh node");
    push.upsert_device(&owner(), fcm_registration("fresh-phone", fresh.node()))
        .await
        .expect("fresh device");
    push.enqueue_notification_publish_job_from_user_server(
        fresh.node(),
        &notification_item("fresh-before-locked-child"),
        &owner(),
    )
    .await
    .expect("fresh work");
    let mut held = fixture.db.begin().await.expect("hold failed provider row");
    let mut rows = held
        .query(
            "SELECT job_id FROM push_publish_jobs WHERE job_id = ? FOR UPDATE",
            crate::db_params![blocked.job.to_string()],
        )
        .await
        .expect("provider row lock");
    assert!(rows.next().await.expect("row").is_some());
    drop(rows);
    let results = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        push.drain_queued_notification_publish_jobs(1),
    )
    .await
    .expect("held failed provider row must not stall fresh work")
    .expect("fresh batch");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].item_id(), "fresh-before-locked-child");
    assert_eq!(sender.sent(), 1);
    assert_eq!(scalar_i64(&push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![blocked.delivery.to_string()]).await, 2, "the locked child is skipped without settling canonical custody");
    assert_failure_history_preserved(&blocked, false).await;
    held.commit().await.expect("release failed provider row");
    assert!(tokio::time::timeout(
        std::time::Duration::from_secs(2),
        push.drain_queued_notification_publish_jobs(1)
    )
    .await
    .expect("released child is repairable")
    .expect("custody repair batch")
    .is_empty());
    assert_eq!(scalar_i64(&push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![blocked.delivery.to_string()]).await, 0);
    assert_eq!(
        sender.sent(),
        1,
        "repair cannot send another provider request"
    );
    assert_failure_history_preserved(&blocked, false).await;
    fixture.close().await;
}

async fn postgres_failed_custody_owner_or_node_lock_is_bounded(owner_lock: bool) {
    use crate::push_service::test_support::{
        fcm_registration, with_fake_fcm, FakeFcmSender, FCM_TEST_APP_ID,
    };
    let schema = if owner_lock {
        "pushcustodyowner"
    } else {
        "pushcustodynode"
    };
    let Some(fixture) = IngressFixture::postgres(schema).await else {
        return;
    };
    let blocked = seed(&fixture, "contended-known-custody-lock").await;
    seed_failed_custody(&blocked, false, 0).await;
    let sender = FakeFcmSender::default();
    let push = with_fake_fcm(blocked.push.clone(), sender.clone());
    let independent_owner: BareJid = "independent@example.com".parse().expect("other owner");
    let fresh = push
        .ensure_node(&independent_owner, FCM_TEST_APP_ID)
        .await
        .expect("independent owner's node");
    push.upsert_device(
        &independent_owner,
        fcm_registration("independent-phone", fresh.node()),
    )
    .await
    .expect("independent device");
    push.enqueue_notification_publish_job_from_user_server(
        fresh.node(),
        &notification_item("independent-before-custody-lock"),
        &independent_owner,
    )
    .await
    .expect("independent work");

    let mut held = fixture.db.begin().await.expect("hold scheduler lock row");
    let (sql, key) = if owner_lock {
        (
            "SELECT owner_bare_jid FROM push_owner_locks WHERE owner_bare_jid = ? FOR UPDATE",
            owner().to_string(),
        )
    } else {
        (
            "SELECT node FROM push_node_locks WHERE node = ? FOR UPDATE",
            blocked.node.node().to_string(),
        )
    };
    let mut rows = held
        .query(sql, crate::db_params![key])
        .await
        .expect("scheduler row lock");
    assert!(rows.next().await.expect("locked row").is_some());
    drop(rows);
    let results = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        push.drain_queued_notification_publish_jobs(1),
    )
    .await
    .expect("owner/node contention is bounded by the ancestry transaction deadlines")
    .expect("independent work completes");
    assert_eq!(results.len(), 1);
    assert_eq!(results[0].item_id(), "independent-before-custody-lock");
    assert_eq!(sender.sent(), 1);
    assert_eq!(scalar_i64(&push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![blocked.delivery.to_string()]).await, 2, "lock contention cannot release canonical custody");
    assert_failure_history_preserved(&blocked, false).await;

    held.commit().await.expect("release scheduler lock row");
    assert!(tokio::time::timeout(
        std::time::Duration::from_secs(2),
        push.drain_queued_notification_publish_jobs(1),
    )
    .await
    .expect("released scheduler lock permits custody repair")
    .expect("custody repair")
    .is_empty());
    assert_eq!(scalar_i64(&push, "SELECT COUNT(*) FROM ingress_effect_descendants WHERE descendant_key = ? AND settled_at IS NULL", crate::db_params![blocked.delivery.to_string()]).await, 0);
    assert_eq!(sender.sent(), 1, "custody repair never replays a send");
    assert_failure_history_preserved(&blocked, false).await;
    fixture.close().await;
}

#[tokio::test]
async fn postgres_failed_custody_owner_lock_does_not_block_independent_owner() {
    postgres_failed_custody_owner_or_node_lock_is_bounded(true).await;
}

#[tokio::test]
async fn postgres_failed_custody_node_lock_does_not_block_independent_owner() {
    postgres_failed_custody_owner_or_node_lock_is_bounded(false).await;
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
    sqlite_known_failure_after_uncertainty_emits_retry_and_parked_dispositions,
    postgres_known_failure_after_uncertainty_emits_retry_and_parked_dispositions,
    known_failure_after_uncertainty_emits_retry_and_parked_dispositions,
    "pushknowndisposition"
);
backend_tests!(
    sqlite_parse_failure_cannot_fail_successor_claim,
    postgres_parse_failure_cannot_fail_successor_claim,
    parse_failure_cannot_fail_successor_claim,
    "pushparse"
);
backend_tests!(
    sqlite_prior_delivery_completes_after_all_devices_become_inactive,
    postgres_prior_delivery_completes_after_all_devices_become_inactive,
    prior_delivery_completes_after_all_devices_become_inactive,
    "pushpriorinactive"
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

backend_tests!(
    sqlite_scheduling_uses_typed_retry_evidence,
    postgres_scheduling_uses_typed_retry_evidence,
    scheduling_uses_typed_retry_evidence,
    "pushtypedretry"
);

backend_tests!(
    sqlite_known_failure_custody_reconciliation_preserves_uncertain_and_history,
    postgres_known_failure_custody_reconciliation_preserves_uncertain_and_history,
    known_failure_custody_reconciliation_preserves_uncertain_and_history,
    "pushcustodyrepair"
);

backend_tests!(
    sqlite_direct_stale_claim_recovery_respects_persisted_exponential_backoff,
    postgres_direct_stale_claim_recovery_respects_persisted_exponential_backoff,
    direct_stale_claim_recovery_respects_persisted_exponential_backoff,
    "pushdirectbackoff"
);
