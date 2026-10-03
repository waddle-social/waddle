//! The live Phase C executor and maintenance share the actual socket fence.
use super::*;
use crate::ingress::{execute::ExternalOutcome, execute_uow::FAIL_DELIVERY_PROGRESS_TX};
use crate::server::routes::interpret::effects::delivery::PeerDeliveryKind;

fn live_submission(
    fixture: &IngressFixture,
    target: &jid::FullJid,
    kind: PeerDeliveryKind,
) -> IngressSubmission {
    let mut submission = direct_submission(fixture, "live-fenced", std::slice::from_ref(target));
    for planned in &mut submission.plan.plan {
        if let Effect::External(ExternalEffect::Delivery(delivery)) = &mut planned.effect {
            let ExternalDeliveryEffect::QueueDetached {
                route_identity,
                stanza,
                ..
            } = delivery
            else {
                panic!("planned direct copy");
            };
            *delivery = ExternalDeliveryEffect::RouteToPeer {
                route_identity: route_identity.clone(),
                jid: target.clone(),
                stanza: stanza.clone(),
                kind,
                call_setup: None,
            };
        }
    }
    submission
}

async fn progress_failure_retry(kind: PeerDeliveryKind, disappear: bool) {
    let fixture = IngressFixture::sqlite().await;
    let sm = persistent_sm(&fixture).await;
    let state = state_for(&fixture, sm.clone()).await;
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("target");
    let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
    socket_tests::register_test_connection(&state, &target, sender).await;
    let submission = live_submission(&fixture, &target, kind);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let key = decision.message_key.expect("key");
    let deps = build_interpret_deps(&state, None);
    let report = FAIL_DELIVERY_PROGRESS_TX
        .scope(
            true,
            execute_effects(
                &fixture.uow,
                &fixture.db,
                &decision,
                &ImmediateSink,
                &deps,
                Duration::from_secs(5),
            ),
        )
        .await;
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Uncertain);
    assert!(receiver.try_recv().is_ok(), "first live copy enqueued");
    assert_eq!(
        fixture.count("ingress_send_attempts WHERE state = 2").await,
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    if disappear {
        state.deps.protocol.connection_registry.unregister(&target);
        store_detached(&sm, &target).await;
    }
    let retry = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(5),
    )
    .await;
    assert_eq!(retry.outcomes[0].1, ExternalOutcome::Done);
    assert!(receiver.try_recv().is_err(), "receipt repair never resends");
    assert_eq!(fixture.count("sm_ingress_appends").await, 0);
    if disappear {
        assert_eq!(
            append_count(&sm, &target).await,
            0,
            "no detached allocation after live acceptance"
        );
    }
    assert_recovered(&fixture, key, 1).await;
    drop(deps);
    state
        .deps
        .protocol
        .ingress
        .drain_and_join(Duration::from_secs(1))
        .await;
    drop(state);
    drop(sm);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_live_direct_receipt_failure_retries_without_second_send() {
    progress_failure_retry(PeerDeliveryKind::DirectFrame, false).await;
}

#[tokio::test]
async fn sqlite_live_peer_receipt_failure_retries_without_second_send() {
    progress_failure_retry(PeerDeliveryKind::PeerStanza, false).await;
}

#[tokio::test]
async fn sqlite_registry_frame_without_archive_positions_retries_without_second_send() {
    progress_failure_retry(PeerDeliveryKind::RegistryFrame, false).await;
}

#[tokio::test]
async fn sqlite_completed_live_send_prevents_detached_retry_after_socket_disappears() {
    progress_failure_retry(PeerDeliveryKind::DirectFrame, true).await;
}

#[tokio::test]
async fn sqlite_live_executor_racing_maintenance_enqueues_once() {
    let fixture = IngressFixture::sqlite().await;
    let sm = persistent_sm(&fixture).await;
    let state = state_for(&fixture, sm.clone()).await;
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("target");
    let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
    socket_tests::register_test_connection(&state, &target, sender).await;
    let submission = live_submission(&fixture, &target, PeerDeliveryKind::DirectFrame);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let key = decision.message_key.expect("key");
    let gate = test_hooks::pause_after_delivery_append(key, target);
    let deps = build_interpret_deps(&state, None);
    let environment: Arc<dyn RecoveryEnvironment> = Arc::new(StateEnvironment(state.clone()));
    let cursor = MaintenanceCursor::default();
    let live = execute_effects(
        &fixture.uow,
        &fixture.db,
        &decision,
        &ImmediateSink,
        &deps,
        Duration::from_secs(20),
    );
    let recover = async {
        gate.wait_until_reached().await;
        assert!(receiver.try_recv().is_ok(), "live executor reached sink");
        assert_eq!(
            pass(&fixture, &environment, &cursor).await,
            MaintenanceOutcome::Complete
        );
        cursor.wait_for_recovery_accounting().await;
        assert_recovered(&fixture, key, 1).await;
        assert!(
            receiver.try_recv().is_err(),
            "maintenance repairs receipt without enqueue"
        );
        gate.release();
    };
    let (report, ()) = tokio::time::timeout(Duration::from_secs(30), async {
        tokio::join!(live, recover)
    })
    .await
    .expect("racing live and maintenance complete");
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Done);
    assert!(receiver.try_recv().is_err());
    assert_eq!(
        fixture.count("ingress_send_attempts WHERE state = 2").await,
        1
    );
    drop(deps);
    drop(environment);
    state
        .deps
        .protocol
        .ingress
        .drain_and_join(Duration::from_secs(1))
        .await;
    drop(state);
    drop(sm);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_admitted_execution_forced_stop_after_start_does_not_enqueue() {
    let fixture = IngressFixture::sqlite().await;
    let sm = persistent_sm(&fixture).await;
    let state = state_for(&fixture, sm.clone()).await;
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("target");
    let (sender, mut receiver) = tokio::sync::mpsc::channel(8);
    socket_tests::register_test_connection(&state, &target, sender).await;
    let submission = live_submission(&fixture, &target, PeerDeliveryKind::DirectFrame);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let key = decision.message_key.expect("key");
    let gate = crate::ingress::live_delivery::test_hooks::pause_after_start(key, target);
    let deps = build_interpret_deps(&state, None);
    let authority = &state.deps.protocol.ingress;
    let force_shutdown = async {
        tokio::time::timeout(Duration::from_secs(1), gate.wait_until_reached())
            .await
            .expect("admitted execution reached durable start");
        assert!(
            !authority.drain_and_join(Duration::from_millis(5)).await,
            "active execution holds admission during shutdown"
        );
        gate.release();
    };
    let (report, ()) = tokio::join!(
        authority.execute(&decision, &ImmediateSink, &deps),
        force_shutdown,
    );
    assert_eq!(report.outcomes[0].1, ExternalOutcome::Uncertain);
    assert!(
        receiver.try_recv().is_err(),
        "forced shutdown fences the actual sink"
    );
    assert_eq!(
        fixture.count("ingress_send_attempts WHERE state = 1").await,
        1
    );
    assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
    drop(deps);
    assert!(authority.drain_and_join(Duration::from_secs(1)).await);
    drop(state);
    drop(sm);
    fixture.close().await;
}

#[tokio::test]
async fn sqlite_local_only_delivery_cannot_fall_through_to_replacement_socket() {
    let fixture = IngressFixture::sqlite().await;
    let target: jid::FullJid = "juliet@example.com/phone".parse().expect("target");
    let registry = waddle_xmpp::registry::ConnectionRegistry::new();
    let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
    registry.register(target.clone(), sender);
    let submission = live_submission(&fixture, &target, PeerDeliveryKind::DirectFrame);
    let decision = commit_submission(&fixture.uow, &submission, 1)
        .await
        .expect("commit");
    let key = decision.message_key.expect("key");
    let gate = crate::ingress::live_delivery::test_hooks::pause_after_start(key, target.clone());
    let mut deps = Deps::new(&registry, "example.com");
    deps.ingress_delivery_uow = Some(fixture.uow.clone());
    deps.ingress_append_context = Some(crate::server::routes::interpret::SmIngressAppendContext {
        message_key: key,
        receipt: decision.route_progress[0].receipt.clone(),
        received_at: None,
        archive_positions: Vec::new(),
        dispatch_stream: None,
    });
    let stanza = Stanza::Message(submission.plan.sanitized_message);
    let (replacement_sender, mut replacement_receiver) = tokio::sync::mpsc::channel(1);
    let replace = async {
        tokio::time::timeout(Duration::from_secs(1), gate.wait_until_reached())
            .await
            .expect("captured owner reaches durable start");
        registry.register(target.clone(), replacement_sender);
        gate.release();
    };
    let (outcome, ()) = tokio::join!(
        crate::server::routes::interpret::deliver_direct_to_full_locally(&deps, &target, &stanza),
        replace,
    );
    assert_eq!(
        outcome,
        crate::server::routes::interpret::FullJidDeliveryOutcome::Unavailable
    );
    assert!(receiver.try_recv().is_err(), "captured socket was replaced");
    assert!(
        replacement_receiver.try_recv().is_err(),
        "no raw-registry retry on the new owner"
    );
    assert_eq!(fixture.count("ingress_send_attempts").await, 0);
    drop(deps);
    fixture.close().await;
}
