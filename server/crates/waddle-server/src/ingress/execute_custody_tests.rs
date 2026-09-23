//! #1760: settled route progress never owns the last recoverable delivery payload.
use super::*;
use crate::ingress::{
    commit::commit_submission, execute_uow::FAIL_DELIVERY_PROGRESS_TX, test_support::IngressFixture,
};
use crate::ingress_uow::DeliveryProgressRepository;
use crate::server::routes::interpret::DeliveryExecutionContext;
use std::sync::Arc;
use waddle_xmpp::{
    ingress::{EffectMessageIdentity, IngressEffectIntent},
    pending_delivery::SmSessionId,
    stream_management::{
        persistence::{IngressCustodyDisposition, SmClaimFence, SmPersistenceStorage},
        DetachedSession, InMemorySmSessionRegistry, SmSessionRegistry,
    },
};

#[derive(Clone, Copy)]
enum CustodyTransition {
    LiveResume,
    ResumePublicationCrash,
    ConcurrentRetirementQuarantine,
    SettledReplayEviction,
}

async fn retirement_authority(fixture: &IngressFixture) -> crate::ingress::IngressAuthority {
    #[cfg(feature = "clustering")]
    if fixture.db.driver() == crate::db::DatabaseDriver::Postgres {
        use waddle_xmpp::ownership::{ClaimStore, NodeIdentity, SharedNodeIdentity};

        crate::clustering::claims::PostgresClaimStore::new(fixture.db.clone())
            .ensure_schema()
            .await
            .expect("retirement claim schema");
        let tx = fixture.uow.begin().await.expect("read attested lineage");
        let crate::ingress_uow::IngressLineage::Attested(lineage) = tx.lineage() else {
            panic!("Postgres fixture must attest its durable lineage");
        };
        let config = crate::config::LineageConfig {
            deployment_uuid: Some(lineage.deployment_uuid),
            action: None,
        };
        tx.commit().await.expect("close lineage read");
        return crate::ingress::IngressAuthority::new(
            Default::default(),
            fixture.db.clone(),
            config,
            Some(SharedNodeIdentity::new(NodeIdentity::new(
                "custody-test",
                "incarnation",
            ))),
        )
        .await
        .expect("node-bound retirement authority");
    }
    fixture.authority().await
}

async fn custody_survives_transition(fixture: IngressFixture, transition: CustodyTransition) {
    let persistence = Arc::new(
        crate::sm_persistence::DatabaseSmPersistence::open(Some(fixture.db.database_url()))
            .await
            .expect("SM persistence"),
    );
    let sm = Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence.clone()));
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("recipient");
    let stream = SmSessionId::new(target.to_string());
    sm.store_session(DetachedSession {
        stream_id: stream.as_str().to_owned(),
        user_id: target.to_bare().to_string(),
        jid: target.clone(),
        occupancy_session: waddle_xmpp_core::OccupancySessionGeneration::mint(),
        inbound_count: 0,
        outbound_count: 0,
        last_acked: 0,
        replay_gap_through: None,
        unacked_stanzas: Vec::new(),
        max_resume_time: Some(300),
        detached_at: std::time::Instant::now(),
        carbons_enabled: false,
        roster_interested: false,
        blocklist_interested: false,
        presence_available: false,
        presence_show: None,
        presence_status: None,
        presence_priority: 0,
        presence_payloads: Vec::new(),
        pending_subscribes_flushed: false,
    })
    .await
    .expect("detached recipient");
    let state = crate::server::routes::websocket::tests::create_test_websocket_state().await;
    let mut deps = Deps::new(&state.deps.protocol.connection_registry, "example.com");
    deps.user_registry = Some(&state.deps.protocol.user_registry);
    deps.sm_session_registry = Some(&sm);
    let mut submission = fixture.submission(Some("custody-transition"), "retained delivery");
    let identity = EffectMessageIdentity::capture_ordinal(1);
    let intent = IngressEffectIntent::RouteDirect {
        recipient: target.to_bare(),
        fanout: vec![target.clone()],
        route_identity: identity.clone(),
    };
    let receipt = crate::ingress::receipt_key(&intent).expect("route receipt");
    submission.plan.intents = vec![intent];
    let sink = crate::server::routes::interpret::effects::PlanSink::new();
    let mut planned = deps.clone();
    planned.effects = &sink;
    crate::server::routes::interpret::effects::delivery::record(
        &planned,
        ExternalDeliveryEffect::QueueDetached {
            route_identity: Some(identity),
            call_setup: None,
            bare: target.to_bare(),
            resources: vec![target.clone()],
            stanza: Box::new(Stanza::Message(submission.plan.sanitized_message.clone())),
        },
    );
    submission.plan.plan = sink.take().0;
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit route");
    let key = decision.message_key.expect("canonical key");
    let settled = matches!(transition, CustodyTransition::SettledReplayEviction);
    let execute = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    );
    let report = FAIL_DELIVERY_PROGRESS_TX.scope(!settled, execute).await;
    assert_eq!(
        report.outcomes[0].1,
        if settled {
            ExternalOutcome::Done
        } else {
            ExternalOutcome::Uncertain
        }
    );
    assert_eq!(
        terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("settlement"),
        settled
    );
    let mut tx = fixture.uow.begin().await.expect("inspect initial progress");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("initial progress"),
        if settled {
            vec![target.clone()]
        } else {
            Vec::new()
        }
    );
    tx.commit().await.expect("read initial progress");
    let pending = persistence
        .list_pending_ingress_appends(10)
        .await
        .expect("pending custody");
    assert_eq!(pending.len(), 1);
    let allocation = pending[0].clone();
    assert_eq!(allocation.key.message_key, key);
    assert_eq!(allocation.key.resource, target);
    assert_eq!(allocation.disposition, IngressCustodyDisposition::Pending);
    let Stanza::Message(payload) = &allocation.payload else {
        panic!("message custody");
    };
    assert_eq!(payload.bodies, submission.plan.sanitized_message.bodies);
    assert_eq!(
        persistence
            .list_unacked(&stream)
            .await
            .expect("queue")
            .len(),
        1
    );

    let mut live_receiver = None;
    match transition {
        CustodyTransition::LiveResume | CustodyTransition::ResumePublicationCrash => {
            let resumed = sm
                .take_session(stream.as_str())
                .await
                .expect("resume deletes snapshot")
                .expect("resumable session");
            assert_eq!(resumed.unacked_stanzas.len(), 1);
            assert!(persistence
                .get_session(&stream)
                .await
                .expect("deleted snapshot")
                .is_none());
            if matches!(transition, CustodyTransition::LiveResume) {
                let (sender, receiver) = tokio::sync::mpsc::channel(4);
                crate::server::routes::websocket::tests::register_test_connection(
                    &state, &target, sender,
                )
                .await;
                live_receiver = Some(receiver);
            }
            // Dropping before publication models a crash after durable snapshot deletion.
            drop(resumed);
        }
        CustodyTransition::ConcurrentRetirementQuarantine => {
            let authority = retirement_authority(&fixture).await;
            authority
                .enroll_stream(&stream)
                .await
                .expect("enroll retirement candidate");
            let fence = SmClaimFence::new(
                waddle_xmpp::ownership::NodeIdentity::new("custody-test", "incarnation"),
                waddle_xmpp::ownership::ClaimEpoch(1),
            );
            let (retirement, quarantine) = tokio::join!(
                authority.forget_stream(&stream),
                persistence.quarantine_session(&stream, &fence),
            );
            assert_eq!(
                retirement.expect("retirement"),
                crate::ingress::IngressRetirementOutcome::Deleted
            );
            quarantine.expect("quarantine");
            assert!(authority
                .lookup_stream(&stream)
                .await
                .expect("retired metadata")
                .is_none());
            assert!(authority.drain_and_join(Duration::from_secs(1)).await);
        }
        CustodyTransition::SettledReplayEviction => {
            // Persist the exact state left by bounded replay eviction. Custody must
            // still be discoverable even though ingress is already terminal.
            let mut snapshot = persistence
                .get_session(&stream)
                .await
                .expect("snapshot")
                .expect("retained snapshot");
            snapshot.replay_gap_through = Some(allocation.sequence);
            persistence
                .store_session_atomic(snapshot, Vec::new())
                .await
                .expect("persist replay eviction");
        }
    }
    assert!(persistence
        .list_unacked(&stream)
        .await
        .expect("evicted or deleted queue")
        .is_empty());
    assert_eq!(
        persistence
            .list_pending_ingress_appends(10)
            .await
            .expect("discover without replay queue"),
        vec![allocation.clone()]
    );
    // A fresh registry rules out an in-memory payload or stale session hiding loss.
    let restarted =
        Arc::new(InMemorySmSessionRegistry::new().with_persistence(persistence.clone()));
    deps.sm_session_registry = Some(&restarted);
    let replay = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("ordinary retry");
    assert_eq!(replay.message_key, Some(key));
    let report = execute_effects(
        &fixture.uow,
        &fixture.db,
        &replay,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert!(report.receipt_failures.is_empty());
    assert!(report
        .outcomes
        .iter()
        .all(|(_, outcome)| *outcome == ExternalOutcome::Done));
    let mut tx = fixture.uow.begin().await.expect("inspect retry progress");
    assert_eq!(
        DeliveryProgressRepository::load(&mut tx, key, &receipt)
            .await
            .expect("progress"),
        vec![target.clone()]
    );
    assert!(EffectReceiptRepository::contains(
        &mut tx,
        key,
        receipt.kind,
        &receipt.semantic_identity_hash
    )
    .await
    .expect("settled receipt"));
    tx.commit().await.expect("read commit");
    assert!(
        terminalize_if_complete(&fixture.uow, key, DeliveryExecutionContext::Live.into())
            .await
            .expect("settled route")
    );
    assert_eq!(
        persistence
            .get_ingress_append(&allocation.key)
            .await
            .expect("immutable proof"),
        Some(allocation.clone())
    );
    assert_eq!(
        persistence
            .list_pending_ingress_appends(10)
            .await
            .expect("payload still recoverable after settlement"),
        vec![allocation.clone()]
    );
    assert_eq!(
        fixture.count("sm_ingress_appends").await,
        1,
        "retry must not allocate again"
    );
    assert!(persistence
        .list_unacked(&stream)
        .await
        .expect("no duplicate queue append")
        .is_empty());
    if let Some(receiver) = &mut live_receiver {
        assert!(
            receiver.try_recv().is_err(),
            "proof-backed retry must not send a second live frame"
        );
    }
    if matches!(
        transition,
        CustodyTransition::ResumePublicationCrash
            | CustodyTransition::ConcurrentRetirementQuarantine
    ) {
        let pending_storage = Arc::new(
            crate::pending_delivery::DatabasePendingDeliveryStorage::open(
                Some(fixture.db.database_url()),
                waddle_xmpp::pending_delivery::QuotaPolicy::default_policy(),
            )
            .await
            .expect("durable pending delivery"),
        );
        let recovery = crate::server::routes::websocket::tests::create_test_websocket_state_with_sm_registry_and_pending_storage(
            restarted,
            pending_storage,
        ).await;
        assert!(
            crate::server::session_janitors::run_ingress_custody_sweep(&recovery, &mut None,).await
        );
        let recovered = recovery
            .deps
            .protocol
            .pending_delivery_storage
            .list(&target.to_bare())
            .await
            .expect("promoted delivery");
        assert_eq!(recovered.len(), 1);
        assert_eq!(
            recovered[0].original_receipt_at,
            allocation.original_receipt_at
        );
        let waddle_xmpp::pending_delivery::PendingPayload::Transient(payload) =
            &recovered[0].payload
        else {
            panic!("recoverable inline message");
        };
        assert_eq!(payload.bodies, submission.plan.sanitized_message.bodies);
        assert!(persistence
            .list_pending_ingress_appends(10)
            .await
            .expect("discharged custody")
            .is_empty());
        let mut promoted = allocation;
        promoted.disposition = IngressCustodyDisposition::Promoted;
        assert_eq!(
            persistence
                .get_ingress_append(&promoted.key)
                .await
                .expect("retained proof"),
            Some(promoted)
        );
        assert!(
            crate::server::session_janitors::run_ingress_custody_sweep(&recovery, &mut None,).await
        );
        assert_eq!(
            recovery
                .deps
                .protocol
                .pending_delivery_storage
                .list(&target.to_bare())
                .await
                .expect("idempotent recovery")
                .len(),
            1
        );
    }
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_custody_survives_live_resume_retry() {
    custody_survives_transition(
        IngressFixture::sqlite().await,
        CustodyTransition::LiveResume,
    )
    .await;
}
#[tokio::test]
async fn postgres_custody_survives_live_resume_retry() {
    if let Some(fixture) = IngressFixture::postgres("custody_live").await {
        custody_survives_transition(fixture, CustodyTransition::LiveResume).await;
    }
}
#[tokio::test]
async fn sqlite_custody_survives_resume_publication_crash() {
    custody_survives_transition(
        IngressFixture::sqlite().await,
        CustodyTransition::ResumePublicationCrash,
    )
    .await;
}
#[tokio::test]
async fn postgres_custody_survives_resume_publication_crash() {
    if let Some(fixture) = IngressFixture::postgres("custody_crash").await {
        custody_survives_transition(fixture, CustodyTransition::ResumePublicationCrash).await;
    }
}
#[tokio::test]
async fn sqlite_custody_survives_concurrent_retirement_quarantine() {
    custody_survives_transition(
        IngressFixture::sqlite().await,
        CustodyTransition::ConcurrentRetirementQuarantine,
    )
    .await;
}
#[tokio::test]
async fn postgres_custody_survives_concurrent_retirement_quarantine() {
    if let Some(fixture) = IngressFixture::postgres("custody_retire").await {
        custody_survives_transition(fixture, CustodyTransition::ConcurrentRetirementQuarantine)
            .await;
    }
}
#[tokio::test]
async fn sqlite_custody_survives_settlement_then_replay_eviction() {
    custody_survives_transition(
        IngressFixture::sqlite().await,
        CustodyTransition::SettledReplayEviction,
    )
    .await;
}
#[tokio::test]
async fn postgres_custody_survives_settlement_then_replay_eviction() {
    if let Some(fixture) = IngressFixture::postgres("custody_evict").await {
        custody_survives_transition(fixture, CustodyTransition::SettledReplayEviction).await;
    }
}
