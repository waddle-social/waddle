//! The live authority retains proven generic receipts after caller cancellation.
#![cfg(feature = "test-support")]

pub mod ingress_support;

use std::time::Duration;

use ingress_support::IngressFixture;
use waddle_server::ingress::commit::commit_submission;
use waddle_server::ingress::effects::direct::ExternalDirectEffect;
use waddle_server::ingress::effects::{Effect, PlanSuppressionPolicy};
use waddle_server::ingress::{
    Deps, ExternalEffect, ExternalOutcome, ImmediateSink, IngressDecision, PlannedEffect,
};
use waddle_server::ingress_uow::EffectReceiptRepository;
use waddle_server::test_support::{
    bind_receipt_custody_fatal_fence, create_ingress_authority_at_startup,
    receipt_custody_capacity, receipt_custody_probe, receipt_custody_shares_ingress_lane,
    websocket_state,
};
use waddle_xmpp::ingress::{
    DmCallThreadKey, IngressEffectIntent, PendingDmCallOffer, PlannedDmCallState,
};
use waddle_xmpp::registry::ConnectionRegistry;
use waddle_xmpp::Stanza;

async fn call_decision(fixture: &IngressFixture) -> (IngressDecision, PlannedDmCallState) {
    call_decision_named(fixture, "receipt-custody").await
}

async fn call_decision_named(
    fixture: &IngressFixture,
    name: &str,
) -> (IngressDecision, PlannedDmCallState) {
    let mut submission = fixture.submission(Some(name), "call offer");
    let key = DmCallThreadKey::new(
        submission.sender.to_bare(),
        "bob@example.com".parse().expect("peer"),
        xmpp_parsers::jingle::SessionId(name.into()),
    );
    let state = PlannedDmCallState {
        key,
        pending: Some(PendingDmCallOffer {
            media: waddle_xmpp::xep::CallThreadMedia::audio_only(),
            initiator: submission.sender.to_bare(),
            started: chrono::Utc::now(),
        }),
        active: None,
        projected: Default::default(),
    };
    let intent = IngressEffectIntent::DmCallThreadState {
        sequence: 0,
        state: Box::new(state.clone()),
    };
    submission.plan.intents = vec![intent.clone()];
    submission.plan.plan = vec![PlannedEffect::new(Effect::External(ExternalEffect::Direct(
        ExternalDirectEffect::DmCallThreadState {
            state: Box::new(state.clone()),
            receipt: Some(Box::new(intent)),
        },
    )))
    .with_suppression(PlanSuppressionPolicy::Always)];
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit call decision");
    assert_eq!(decision.external_receipts[0].len(), 1);
    (decision, state)
}

async fn three_call_decision(fixture: &IngressFixture) -> IngressDecision {
    let mut submission = fixture.submission(Some("shared-receipt-custody"), "shared keys");
    let mut effects = Vec::new();
    let mut intents = Vec::new();
    for sequence in 0..3 {
        let state = PlannedDmCallState {
            key: DmCallThreadKey::new(
                submission.sender.to_bare(),
                "bob@example.com".parse().expect("peer"),
                xmpp_parsers::jingle::SessionId("shared-call".into()),
            ),
            pending: None,
            active: None,
            projected: Default::default(),
        };
        let intent = IngressEffectIntent::DmCallThreadState {
            sequence,
            state: Box::new(state.clone()),
        };
        effects.push(
            PlannedEffect::new(Effect::External(ExternalEffect::Direct(
                ExternalDirectEffect::DmCallThreadState {
                    state: Box::new(state),
                    receipt: Some(Box::new(intent.clone())),
                },
            )))
            .with_suppression(PlanSuppressionPolicy::Always),
        );
        intents.push(intent);
    }
    submission.plan.plan = effects;
    submission.plan.intents = intents;
    commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit three distinct receipt intents")
}

async fn cancellation_after_proof(fixture: IngressFixture) {
    let authority = fixture.authority().await;
    let socket = websocket_state().await;
    bind_receipt_custody_fatal_fence(&authority, &socket);
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&socket);
    let (decision, state) = call_decision(&fixture).await;
    let probe = receipt_custody_probe(&authority, decision.message_key.expect("message"), false);
    let mut execution = Box::pin(authority.execute(&decision, &ImmediateSink, &deps));
    tokio::select! {
        _ = probe.wait_for_attempt() => {},
        _ = &mut execution => panic!("execution completed before receipt custody"),
    }
    assert!(socket
        .deps
        .protocol
        .pending_dm_call_offers
        .contains_key(&state.key));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    drop(execution);
    probe.release_attempt();
    assert!(authority.drain_and_join(Duration::from_secs(10)).await);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        0,
        "receipt continuation does not advance the message frontier"
    );
    drop(deps);
    drop(socket);
    drop(authority);
    fixture.close().await;
}

async fn lost_ack_retries_exact_insert(fixture: IngressFixture) {
    let authority = fixture.authority().await;
    let socket = websocket_state().await;
    bind_receipt_custody_fatal_fence(&authority, &socket);
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&socket);
    let (decision, state) = call_decision(&fixture).await;
    let probe = receipt_custody_probe(&authority, decision.message_key.expect("message"), true);
    let mut execution = Box::pin(authority.execute(&decision, &ImmediateSink, &deps));
    tokio::select! {
        _ = probe.wait_for_attempt() => {},
        _ = &mut execution => panic!("execution completed before first insert"),
    }
    probe.release_attempt();
    tokio::time::timeout(Duration::from_secs(10), probe.wait_for_attempt())
        .await
        .expect("uncertain acknowledgement retries");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        0
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut execution)
            .await
            .is_err()
    );
    socket
        .deps
        .protocol
        .pending_dm_call_offers
        .remove(&state.key);
    probe.release_attempt();
    let report = tokio::time::timeout(Duration::from_secs(10), execution)
        .await
        .expect("retry completed");
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert!(report.receipt_failures.is_empty());
    assert!(
        !socket
            .deps
            .protocol
            .pending_dm_call_offers
            .contains_key(&state.key),
        "retry never runs the effect again"
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(authority.drain_and_join(Duration::from_secs(10)).await);
    drop(deps);
    drop(socket);
    drop(authority);
    fixture.close().await;
}

async fn fatal_fence_before_insert(fixture: IngressFixture) {
    let authority = fixture.authority().await;
    let socket = websocket_state().await;
    bind_receipt_custody_fatal_fence(&authority, &socket);
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&socket);
    let (decision, _) = call_decision(&fixture).await;
    let probe = receipt_custody_probe(&authority, decision.message_key.expect("message"), false);
    let mut execution = Box::pin(authority.execute(&decision, &ImmediateSink, &deps));
    tokio::select! {
        _ = probe.wait_for_attempt() => {},
        _ = &mut execution => panic!("execution completed before receipt custody"),
    }
    drop(execution);
    // Model the production shutdown selector dropping a pending drain when
    // fatal fencing wins, before the drain's own timeout can force-stop it.
    let mut drain = Box::pin(authority.drain_and_join(Duration::from_secs(10)));
    assert!(tokio::time::timeout(Duration::from_millis(10), &mut drain)
        .await
        .is_err());
    drop(drain);
    socket
        .deps
        .app_state
        .node_lifecycle
        .fail(waddle_server::clustering::CriticalNodeFailure::UserRegistryTerminated);
    probe.release_attempt();
    assert!(!authority.drain_and_join(Duration::from_secs(10)).await);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    drop(deps);
    drop(socket);
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_fatal_fence_stops_retained_insert() {
    fatal_fence_before_insert(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_fatal_fence_stops_retained_insert() {
    if let Some(fixture) = IngressFixture::postgres("receipt_custody_fatal").await {
        fatal_fence_before_insert(fixture).await;
    }
}

#[tokio::test]
async fn fatal_fence_stops_retry_after_simulated_lost_ack() {
    let fixture = IngressFixture::sqlite().await;
    fixture
        .execute("CREATE TABLE receipt_insert_attempts (id INTEGER)", ())
        .await;
    fixture.execute("CREATE TRIGGER count_receipt_attempt BEFORE INSERT ON ingress_effect_receipts BEGIN INSERT INTO receipt_insert_attempts (id) VALUES (1); END", ()).await;
    let authority = fixture.authority().await;
    let socket = websocket_state().await;
    bind_receipt_custody_fatal_fence(&authority, &socket);
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&socket);
    let (decision, _) = call_decision(&fixture).await;
    let probe = receipt_custody_probe(&authority, decision.message_key.expect("message"), true);
    let mut execution = Box::pin(authority.execute(&decision, &ImmediateSink, &deps));
    tokio::select! {
        _ = probe.wait_for_attempt() => {},
        _ = &mut execution => panic!("execution completed before first insert"),
    }
    probe.release_attempt();
    tokio::time::timeout(Duration::from_secs(10), probe.wait_for_attempt())
        .await
        .expect("simulated lost acknowledgement schedules a retry");
    assert_eq!(fixture.count("receipt_insert_attempts").await, 1);
    drop(execution);
    socket
        .deps
        .app_state
        .node_lifecycle
        .fail(waddle_server::clustering::CriticalNodeFailure::UserRegistryTerminated);
    probe.release_attempt();
    assert!(!authority.drain_and_join(Duration::from_secs(10)).await);
    assert_eq!(fixture.count("receipt_insert_attempts").await, 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    drop(deps);
    drop(socket);
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn missing_fatal_fence_uses_existing_executor() {
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let socket = websocket_state().await;
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&socket);
    let (decision, _) = call_decision(&fixture).await;
    receipt_custody_capacity(&authority, 0, 0);
    let report = authority.execute(&decision, &ImmediateSink, &deps).await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert!(report.receipt_failures.is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(authority.drain_and_join(Duration::from_secs(10)).await);
    drop(deps);
    drop(socket);
    drop(authority);
    fixture.close().await;
}

#[cfg(feature = "clustering")]
#[tokio::test]
async fn node_bound_clustered_authority_uses_existing_executor() {
    use waddle_server::config::LineageConfig;
    use waddle_server::ingress::IngressAuthority;
    use waddle_server::ingress_uow::IngressLineage;
    use waddle_xmpp::ownership::{NodeIdentity, SharedNodeIdentity};

    let Some(fixture) = IngressFixture::postgres("receipt_custody_clustered").await else {
        return;
    };
    let tx = fixture.uow.begin().await.expect("read attested lineage");
    let IngressLineage::Attested(lineage) = tx.lineage() else {
        panic!("PostgreSQL fixture must attest lineage");
    };
    let lineage = LineageConfig {
        deployment_uuid: Some(lineage.deployment_uuid),
        action: None,
    };
    tx.commit().await.expect("close lineage read");
    let authority = IngressAuthority::try_new(
        Default::default(),
        fixture.db.clone(),
        lineage,
        Some(SharedNodeIdentity::new(NodeIdentity::new(
            "custody-cluster-test",
            "incarnation",
        ))),
    )
    .await
    .expect("node-bound authority");
    let socket = websocket_state().await;
    bind_receipt_custody_fatal_fence(&authority, &socket);
    receipt_custody_capacity(&authority, 0, 0);
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&socket);
    let (decision, _) = call_decision(&fixture).await;
    let report = authority.execute(&decision, &ImmediateSink, &deps).await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert!(report.receipt_failures.is_empty());
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(authority.drain_and_join(Duration::from_secs(10)).await);
    drop(deps);
    drop(socket);
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_caller_cancellation_after_proof() {
    cancellation_after_proof(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_caller_cancellation_after_proof() {
    if let Some(fixture) = IngressFixture::postgres("receipt_custody_cancel").await {
        cancellation_after_proof(fixture).await;
    }
}

#[tokio::test]
async fn postgres_owner_shared_ingress_lane_continues_after_caller_cancellation() {
    use waddle_server::config::{IngressConfig, LineageConfig};
    use waddle_server::db::bootstrap::bootstrap_shared_owner_postgres;
    use waddle_server::db::{DatabaseConfig, DatabaseDriver};
    use waddle_server::ingress_uow::IngressLineage;

    let Some(fixture) = IngressFixture::postgres("receipt_custody_owner_shared").await else {
        return;
    };
    let tx = fixture.uow.begin().await.expect("read attested lineage");
    let IngressLineage::Attested(lineage) = tx.lineage() else {
        panic!("PostgreSQL fixture must attest lineage");
    };
    let lineage = LineageConfig {
        deployment_uuid: Some(lineage.deployment_uuid),
        action: None,
    };
    tx.commit().await.expect("close lineage read");
    let ingress_config = IngressConfig::default();
    let config = DatabaseConfig::new(DatabaseDriver::Postgres, fixture.db.database_url());
    let (pool, _, outbox) = bootstrap_shared_owner_postgres(
        config,
        &lineage,
        false,
        b"test-key",
        1,
        ingress_config.pool_size,
    )
    .await
    .expect("bootstrap and seal shared owner pool");
    let authority = create_ingress_authority_at_startup(&ingress_config, &pool, &lineage)
        .await
        .expect("select owner-shared ingress authority");
    let ingress = pool.ingress();
    assert!(receipt_custody_shares_ingress_lane(&authority, &ingress));
    assert!(!receipt_custody_shares_ingress_lane(
        &authority,
        pool.global()
    ));
    let socket = websocket_state().await;
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&socket);

    // Saturate the one-slot main lane. Both the ordinary executor and frame
    // completion must still persist through the authority's reserved lane.
    let (generic, _) = call_decision_named(&fixture, "writer-generic-lane").await;
    let (mut frame, _) = call_decision_named(&fixture, "writer-frame-lane").await;
    frame.external[0] = ExternalEffect::Frame(Box::new(Stanza::Message(
        xmpp_parsers::message::Message::new(None),
    )));
    let main = pool.global().begin().await.expect("occupy main lane");
    let generic_report = tokio::time::timeout(
        Duration::from_secs(2),
        authority.execute(&generic, &ImmediateSink, &deps),
    )
    .await
    .expect("generic receipt must not wait for the main lane");
    assert_eq!(generic_report.outcomes[0].1, ExternalOutcome::Done);
    assert!(generic_report.receipt_failures.is_empty());
    let mut frame_report = tokio::time::timeout(
        Duration::from_secs(2),
        authority.execute(&frame, &ImmediateSink, &deps),
    )
    .await
    .expect("prepare frame on the ingress lane");
    assert_eq!(
        frame_report.outcomes[0].1,
        ExternalOutcome::AwaitingFrameDelivery
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    assert!(tokio::time::timeout(
        Duration::from_secs(2),
        authority.complete_frame_obligations(&mut frame_report),
    )
    .await
    .expect("frame receipt must not wait for the main lane")
    .expect("complete written frame"));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    main.rollback().await.expect("release main lane");

    bind_receipt_custody_fatal_fence(&authority, &socket);
    let (decision, state) = call_decision(&fixture).await;
    let message = decision.message_key.expect("message");
    let key = &decision.external_receipts[0][0];
    let probe = receipt_custody_probe(&authority, message, false);
    let mut execution = Box::pin(authority.execute(&decision, &ImmediateSink, &deps));
    tokio::select! {
        _ = probe.wait_for_attempt() => {},
        _ = &mut execution => panic!("execution completed before receipt custody"),
    }
    assert!(socket
        .deps
        .protocol
        .pending_dm_call_offers
        .contains_key(&state.key));
    drop(execution);
    probe.release_attempt();
    assert!(authority.drain_and_join(Duration::from_secs(10)).await);
    let mut tx = fixture.uow.begin().await.expect("inspect exact receipt");
    assert!(EffectReceiptRepository::contains(
        &mut tx,
        message,
        key.kind,
        &key.semantic_identity_hash,
    )
    .await
    .expect("receipt through the shared ingress lane"));
    tx.commit().await.expect("close receipt read");
    drop(deps);
    drop(socket);
    drop(authority);
    drop(ingress);
    drop(outbox);
    drop(pool);
    fixture.close().await;
}

#[tokio::test]
async fn frame_receipt_partial_progress_retries_without_rewriting_frames() {
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let registry = ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    let mut decision = three_call_decision(&fixture).await;
    decision.external.truncate(2);
    decision.external_dependencies.truncate(2);
    decision.external_receipts.truncate(2);
    for effect in &mut decision.external {
        *effect = ExternalEffect::Frame(Box::new(Stanza::Message(
            xmpp_parsers::message::Message::new(None),
        )));
    }
    fixture
        .execute(
            "CREATE TRIGGER fail_second_frame_receipt BEFORE INSERT ON ingress_effect_receipts \
             WHEN (SELECT COUNT(*) FROM ingress_effect_receipts) >= 1 \
             BEGIN SELECT RAISE(FAIL, 'second frame receipt'); END",
            (),
        )
        .await;
    let mut report = authority.execute(&decision, &ImmediateSink, &deps).await;
    assert!(report
        .outcomes
        .iter()
        .all(|(_, outcome)| *outcome == ExternalOutcome::AwaitingFrameDelivery));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert!(matches!(
        authority.complete_frame_obligations(&mut report).await,
        Err(waddle_server::ingress::execute::ExecutionPersistenceFailure::Storage(_))
    ));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    let message = decision.message_key.expect("message");
    let first = &decision.external_receipts[0][0];
    let second = &decision.external_receipts[1][0];
    let mut tx = fixture.uow.begin().await.expect("inspect partial receipts");
    assert!(EffectReceiptRepository::contains(
        &mut tx,
        message,
        first.kind,
        &first.semantic_identity_hash,
    )
    .await
    .expect("inspect first receipt"));
    assert!(!EffectReceiptRepository::contains(
        &mut tx,
        message,
        second.kind,
        &second.semantic_identity_hash,
    )
    .await
    .expect("inspect second receipt"));
    tx.commit().await.expect("close partial receipt inspection");
    assert!(report
        .outcomes
        .iter()
        .all(|(_, outcome)| *outcome == ExternalOutcome::AwaitingFrameDelivery));
    fixture
        .execute("DROP TRIGGER fail_second_frame_receipt", ())
        .await;
    assert!(!authority
        .complete_frame_obligations(&mut report)
        .await
        .expect("retry written-frame receipts"));
    assert!(report
        .outcomes
        .iter()
        .all(|(_, outcome)| *outcome == ExternalOutcome::Done));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert!(!authority
        .complete_frame_obligations(&mut report)
        .await
        .expect("receipt retry is idempotent"));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert!(authority.drain_and_join(Duration::from_secs(10)).await);
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_uncertain_ack_retries_only_receipt() {
    lost_ack_retries_exact_insert(IngressFixture::sqlite().await).await;
}

#[tokio::test]
async fn postgres_uncertain_ack_retries_only_receipt() {
    if let Some(fixture) = IngressFixture::postgres("receipt_custody_ack").await {
        lost_ack_retries_exact_insert(fixture).await;
    }
}

#[tokio::test]
async fn capacity_rejection_leaves_effect_and_frontier_uncertain() {
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let socket = websocket_state().await;
    bind_receipt_custody_fatal_fence(&authority, &socket);
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&socket);
    let (decision, state) = call_decision(&fixture).await;
    receipt_custody_capacity(&authority, 0, 0);
    let report = authority.execute(&decision, &ImmediateSink, &deps).await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Uncertain);
    assert_eq!(report.receipt_failures.len(), 1);
    assert!(socket
        .deps
        .protocol
        .pending_dm_call_offers
        .contains_key(&state.key));
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        0
    );
    assert!(authority.drain_and_join(Duration::from_secs(10)).await);
    drop(deps);
    drop(socket);
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn forced_drain_never_reports_abandoned_proof_as_drained() {
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let socket = websocket_state().await;
    bind_receipt_custody_fatal_fence(&authority, &socket);
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&socket);
    let (decision, _) = call_decision(&fixture).await;
    let probe = receipt_custody_probe(&authority, decision.message_key.expect("message"), false);
    let mut execution = Box::pin(authority.execute(&decision, &ImmediateSink, &deps));
    tokio::select! {
        _ = probe.wait_for_attempt() => {},
        _ = &mut execution => panic!("execution completed before receipt custody"),
    }
    drop(execution);
    assert!(!authority.drain_and_join(Duration::from_millis(10)).await);
    probe.release_attempt();
    assert!(!authority.drain_and_join(Duration::from_secs(10)).await);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    drop(deps);
    drop(socket);
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn graceful_drain_waits_for_admitted_effect_to_transfer_proof() {
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let socket = websocket_state().await;
    bind_receipt_custody_fatal_fence(&authority, &socket);
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&socket);
    let (decision, _) = call_decision(&fixture).await;
    let probe = receipt_custody_probe(&authority, decision.message_key.expect("message"), false);
    probe.pause_before_effect();
    let mut execution = Box::pin(authority.execute(&decision, &ImmediateSink, &deps));
    tokio::select! {
        _ = probe.wait_before_effect() => {},
        _ = &mut execution => panic!("execution completed before effect gate"),
    }
    let mut drain = Box::pin(authority.drain_and_join(Duration::from_secs(10)));
    assert!(tokio::time::timeout(Duration::from_millis(10), &mut drain)
        .await
        .is_err());
    probe.release_effect();
    tokio::select! {
        _ = probe.wait_for_attempt() => {},
        _ = &mut execution => panic!("admitted effect lost its proof during drain"),
    }
    assert!(tokio::time::timeout(Duration::from_millis(10), &mut drain)
        .await
        .is_err());
    probe.release_attempt();
    let report = execution.await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert!(report.receipt_failures.is_empty());
    assert!(drain.await);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    drop(deps);
    drop(socket);
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn rejected_receipt_preserves_an_earlier_frame_completion() {
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let socket = websocket_state().await;
    bind_receipt_custody_fatal_fence(&authority, &socket);
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&socket);
    let mut decision = three_call_decision(&fixture).await;
    let frame_key = decision.external_receipts[0][0].clone();
    let later_key = decision.external_receipts[1][0].clone();
    decision.external.truncate(2);
    decision.external[0] = ExternalEffect::Frame(Box::new(Stanza::Message(
        xmpp_parsers::message::Message::new(None),
    )));
    decision.external_dependencies.truncate(2);
    decision.external_receipts.truncate(2);
    receipt_custody_capacity(&authority, 0, 0);
    let mut report = authority.execute(&decision, &ImmediateSink, &deps).await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::AwaitingFrameDelivery);
    assert_eq!(report.outcomes[1].1, ExternalOutcome::Uncertain);
    assert_eq!(report.frame_obligations.len(), 1);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    assert!(!report
        .complete_frame_obligations(&fixture.uow, &fixture.db, Duration::from_secs(5))
        .await
        .expect("receipt the written frame"));
    let mut tx = fixture.uow.begin().await.expect("inspect receipts");
    let message = decision.message_key.expect("message");
    assert!(EffectReceiptRepository::contains(
        &mut tx,
        message,
        frame_key.kind,
        &frame_key.semantic_identity_hash,
    )
    .await
    .expect("frame receipt"));
    assert!(!EffectReceiptRepository::contains(
        &mut tx,
        message,
        later_key.kind,
        &later_key.semantic_identity_hash,
    )
    .await
    .expect("rejected receipt"));
    tx.commit().await.expect("close inspection");
    assert!(authority.drain_and_join(Duration::from_secs(10)).await);
    drop(deps);
    drop(socket);
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn rejected_receipt_does_not_skip_a_later_independent_frame() {
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let socket = websocket_state().await;
    bind_receipt_custody_fatal_fence(&authority, &socket);
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&socket);
    let mut decision = three_call_decision(&fixture).await;
    let frame_key = decision.external_receipts[1][0].clone();
    decision.external.truncate(2);
    decision.external[1] = ExternalEffect::Frame(Box::new(Stanza::Message(
        xmpp_parsers::message::Message::new(None),
    )));
    decision.external_dependencies.truncate(2);
    decision.external_receipts.truncate(2);
    receipt_custody_capacity(&authority, 0, 0);
    let mut report = authority.execute(&decision, &ImmediateSink, &deps).await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Uncertain);
    assert_eq!(report.outcomes[1].1, ExternalOutcome::AwaitingFrameDelivery);
    assert_eq!(report.frame_obligations.len(), 1);
    assert!(!report
        .complete_frame_obligations(&fixture.uow, &fixture.db, Duration::from_secs(5))
        .await
        .expect("receipt the later written frame"));
    let mut tx = fixture.uow.begin().await.expect("inspect frame receipt");
    assert!(EffectReceiptRepository::contains(
        &mut tx,
        decision.message_key.expect("message"),
        frame_key.kind,
        &frame_key.semantic_identity_hash,
    )
    .await
    .expect("later frame receipt"));
    tx.commit().await.expect("close inspection");
    assert!(authority.drain_and_join(Duration::from_secs(10)).await);
    drop(deps);
    drop(socket);
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn postgres_blocked_receipt_does_not_stall_a_healthy_key() {
    let Some(fixture) = IngressFixture::postgres("receipt_custody_independence").await else {
        return;
    };
    let authority = fixture.authority().await;
    let socket = websocket_state().await;
    bind_receipt_custody_fatal_fence(&authority, &socket);
    let registry = ConnectionRegistry::new();
    let mut deps = Deps::new(&registry, "example.com");
    deps.web_socket_state = Some(&socket);
    let (blocked, _) = call_decision_named(&fixture, "blocked-custody").await;
    let (healthy, _) = call_decision_named(&fixture, "healthy-custody").await;
    let key = blocked.external_receipts[0][0].clone();
    let message = blocked.message_key.expect("blocked message");
    let mut blocker = fixture
        .db
        .begin_immediate()
        .await
        .expect("blocking transaction");
    waddle_server::ingress_substrate::record_receipt(
        &mut blocker,
        message,
        key.kind,
        &key.semantic_identity_hash,
    )
    .await
    .expect("hold uncommitted unique receipt key");
    let blocker_pid = {
        let mut rows = blocker
            .query("SELECT pg_backend_pid()", ())
            .await
            .expect("blocker pid query");
        rows.next()
            .await
            .expect("blocker pid row")
            .expect("blocker pid exists")
            .get::<i64>(0)
            .expect("blocker pid")
    };
    let probe = receipt_custody_probe(&authority, message, false);
    let mut waiting = Box::pin(authority.execute(&blocked, &ImmediateSink, &deps));
    tokio::select! {
        _ = probe.wait_for_attempt() => {},
        _ = &mut waiting => panic!("blocked receipt completed before insertion"),
    }
    probe.stop_pausing_attempts();
    probe.release_attempt();
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let sql = format!(
                "pg_stat_activity WHERE {blocker_pid} = ANY(pg_blocking_pids(pid)) \
                 AND query LIKE 'INSERT INTO ingress_effect_receipts%'"
            );
            if fixture.count(&sql).await > 0 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("receipt insert waits on the exact locked key");
    let result = tokio::time::timeout(
        Duration::from_secs(2),
        authority.execute(&healthy, &ImmediateSink, &deps),
    )
    .await
    .expect("independent key must complete before blocker release");
    assert_eq!(result.outcomes[0].1, ExternalOutcome::Done);
    assert!(result.receipt_failures.is_empty());
    assert!(
        tokio::time::timeout(Duration::from_millis(10), &mut waiting)
            .await
            .is_err()
    );
    blocker.commit().await.expect("release locked key");
    let result = tokio::time::timeout(Duration::from_secs(5), waiting)
        .await
        .expect("blocked key completes after release");
    assert_eq!(result.outcomes[0].1, ExternalOutcome::Done);
    assert!(authority.drain_and_join(Duration::from_secs(10)).await);
    drop(deps);
    drop(socket);
    drop(authority);
    fixture.close().await;
}

#[tokio::test]
async fn shared_multi_key_admission_is_all_or_none_and_excludes_arm_owned_receipts() {
    let fixture = IngressFixture::sqlite().await;
    let authority = fixture.authority().await;
    let socket = websocket_state().await;
    bind_receipt_custody_fatal_fence(&authority, &socket);
    let registry = ConnectionRegistry::new();
    let deps = Deps::new(&registry, "example.com");
    let mut decision = three_call_decision(&fixture).await;
    let keys: Vec<_> = decision
        .external_receipts
        .iter()
        .map(|keys| keys.first().expect("receipt key").clone())
        .collect();
    assert_eq!(keys.len(), 3);
    assert_ne!(keys[0], keys[1]);
    let scrub = ExternalEffect::Direct(ExternalDirectEffect::ScrubReplayForTombstone {
        target: waddle_xmpp::tombstone::TombstoneTarget::Groupchat {
            stanza_id: "frozen".into(),
            room: "room@muc.example.com".parse().expect("room"),
        },
    });
    // Synthetic mapping exercises the executor's all-sibling proof rule with
    // a multi-key batch while keeping real canonical receipt rows underneath.
    decision.external = vec![scrub.clone(), scrub];
    decision.external_dependencies = vec![Vec::new(), Vec::new()];
    decision.external_receipts = vec![keys.clone(), keys.clone()];
    decision.arm_owned_receipts = vec![keys[2].clone()];
    decision.receipts_pending = keys.clone();
    receipt_custody_capacity(&authority, 1, 1);
    let rejected = authority.execute(&decision, &ImmediateSink, &deps).await;
    assert_eq!(rejected.outcomes[0].1, ExternalOutcome::Done);
    assert_eq!(rejected.outcomes[1].1, ExternalOutcome::Uncertain);
    assert_eq!(rejected.receipt_failures.len(), 2);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    receipt_custody_capacity(&authority, 1, 2);
    let probe = receipt_custody_probe(&authority, decision.message_key.expect("message"), false);
    let mut execution = Box::pin(authority.execute(&decision, &ImmediateSink, &deps));
    tokio::select! {
        _ = probe.wait_for_attempt() => {},
        _ = &mut execution => panic!("execution completed before first insert"),
    }
    drop(execution);
    probe.release_attempt();
    tokio::time::timeout(Duration::from_secs(10), probe.wait_for_attempt())
        .await
        .expect("the second retained key survives caller cancellation");
    assert_eq!(fixture.count("ingress_effect_receipts").await, 1);
    probe.release_attempt();
    assert!(authority.drain_and_join(Duration::from_secs(10)).await);
    assert_eq!(fixture.count("ingress_effect_receipts").await, 2);
    assert_eq!(
        fixture
            .count("ingress_messages WHERE terminal_at IS NOT NULL")
            .await,
        0
    );
    drop(authority);
    fixture.close().await;
}
