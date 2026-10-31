use super::{
    ClusteringShutdown, CriticalNodeFailure, NodeAdmission, NodeAdmissionError, NodeLifecycle,
    StartupServingTransition,
};

#[tokio::test(start_paused = true)]
async fn clustering_shutdown_deadline_stops_waiting_without_cancelling_the_task() {
    let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = tokio::sync::oneshot::channel();
    let (completed_tx, mut completed_rx) = tokio::sync::oneshot::channel();
    let exited = tokio_util::sync::CancellationToken::new();
    let exit_guard = exited.clone().drop_guard();
    let task = tokio::spawn(async move {
        let _exit_guard = exit_guard;
        entered_tx.send(()).expect("observer remains alive");
        release_rx.await.expect("test releases the pending task");
        completed_tx
            .send(())
            .expect("completion observer remains alive");
    });
    tokio::time::timeout(std::time::Duration::from_secs(1), entered_rx)
        .await
        .expect("task starts promptly")
        .expect("task starts before the deadline");

    let deadline = tokio::time::Instant::now() + std::time::Duration::from_secs(5);
    let wait = ClusteringShutdown(Some(task)).await_drain_until(deadline);
    tokio::pin!(wait);
    assert!(futures::poll!(wait.as_mut()).is_pending());
    tokio::time::advance(std::time::Duration::from_millis(4_999)).await;
    assert!(
        futures::poll!(wait.as_mut()).is_pending(),
        "the outer wait must not finish early"
    );
    tokio::time::advance(std::time::Duration::from_millis(1)).await;
    tokio::time::timeout(std::time::Duration::from_secs(1), wait)
        .await
        .expect("the outer wait returns at its deadline");
    assert_eq!(tokio::time::Instant::now(), deadline);
    assert!(!exited.is_cancelled(), "the task is still running");
    assert_eq!(
        completed_rx.try_recv(),
        Err(tokio::sync::oneshot::error::TryRecvError::Empty),
        "timeout is not task completion or cancellation"
    );

    // Explicitly release and observe the detached task's exit: dropping its
    // join handle must not strand it or silently cancel its pending work.
    release_tx
        .send(())
        .expect("the detached task is still alive");
    tokio::time::timeout(std::time::Duration::from_secs(1), async {
        completed_rx.await.expect("the detached task completes");
        exited.cancelled().await;
    })
    .await
    .expect("the detached task exits after release");
}

#[test]
fn latched_startup_block_refuses_every_serving_promotion() {
    let lifecycle = NodeLifecycle::starting();
    lifecycle.latch_startup_block();

    assert!(matches!(
        lifecycle.finish_startup(),
        StartupServingTransition::Blocked(NodeAdmission::Starting)
    ));
    // The clustering lease-recovery path promotes via `serve()`
    // unconditionally; the latch must hold there too.
    lifecycle.serve();
    assert_eq!(lifecycle.admission(), NodeAdmission::Starting);
    assert!(lifecycle.admit().is_err());
    assert!(lifecycle.startup_blocked());
}

#[test]
fn latch_after_racing_serve_demotes_and_revokes_the_generation() {
    // Clustering lease recovery can call `serve()` BEFORE the startup
    // attestation gate runs. Latching afterwards must demote the node
    // and revoke any permit issued by the racing generation.
    let lifecycle = NodeLifecycle::starting();
    lifecycle.serve();
    let permit = lifecycle.admit().expect("racing promotion admits");

    lifecycle.latch_startup_block();
    assert_eq!(lifecycle.admission(), NodeAdmission::Starting);
    assert!(permit.revalidate().is_err());
    assert!(lifecycle.admit().is_err());
    // And it stays down: another recovery serve() cannot re-promote.
    lifecycle.serve();
    assert_eq!(lifecycle.admission(), NodeAdmission::Starting);
}

#[test]
fn failed_latch_overrides_every_later_nonterminal_transition() {
    let lifecycle = NodeLifecycle::starting();
    lifecycle.fail(CriticalNodeFailure::RoomRegistryTerminated);
    lifecycle.serve();
    lifecycle.begin_drain();
    lifecycle.serve();
    assert_eq!(
        lifecycle.admission(),
        NodeAdmission::Failed(CriticalNodeFailure::RoomRegistryTerminated)
    );
    assert!(!lifecycle.is_ready());
}

#[test]
fn only_serving_nodes_issue_admission_permits() {
    let lifecycle = NodeLifecycle::starting();
    assert!(lifecycle.admit().is_err());
    lifecycle.serve();
    let permit = lifecycle.admit().expect("serving permit");
    assert!(permit.revalidate().is_ok());
    lifecycle.begin_fenced_recovery();
    assert!(lifecycle.admit().is_err());
    assert_eq!(
        permit.revalidate(),
        Err(NodeAdmissionError::NotServing(
            NodeAdmission::FencedRecovering
        ))
    );

    lifecycle.serve();
    assert_eq!(permit.revalidate(), Err(NodeAdmissionError::Revoked));
    assert!(lifecycle
        .admit()
        .expect("recovered serving permit")
        .revalidate()
        .is_ok());
}

#[tokio::test]
async fn old_generation_revocation_stays_latched_after_recovery() {
    let lifecycle = NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");

    lifecycle.begin_fenced_recovery();
    lifecycle.serve();

    tokio::time::timeout(std::time::Duration::from_millis(50), permit.revoked())
        .await
        .expect("old permit must remain revoked after recovery");
    assert_eq!(permit.revalidate(), Err(NodeAdmissionError::Revoked));
}

#[tokio::test]
async fn lifecycle_revocation_wins_over_ready_socket_work() {
    let lifecycle = NodeLifecycle::new();
    let permit = lifecycle.admit().expect("serving permit");
    lifecycle.begin_fenced_recovery();
    let mut dispatched = false;

    tokio::select! {
        biased;
        _ = permit.revoked() => {}
        _ = std::future::ready(()) => dispatched = true,
    }

    assert!(
        !dispatched,
        "no stanza may dispatch after generation revocation"
    );
}

#[test]
fn startup_promotion_cannot_overwrite_fencing_or_failure() {
    let lifecycle = NodeLifecycle::starting();
    assert_eq!(
        lifecycle.finish_startup(),
        StartupServingTransition::Promoted
    );
    assert_eq!(
        lifecycle.finish_startup(),
        StartupServingTransition::AlreadyServing
    );

    let fenced = NodeLifecycle::starting();
    fenced.begin_fenced_recovery();
    assert_eq!(
        fenced.finish_startup(),
        StartupServingTransition::Blocked(NodeAdmission::FencedRecovering)
    );
    assert_eq!(fenced.admission(), NodeAdmission::FencedRecovering);

    let failed = NodeLifecycle::starting();
    failed.fail(CriticalNodeFailure::UserRegistryTerminated);
    assert_eq!(
        failed.finish_startup(),
        StartupServingTransition::Blocked(NodeAdmission::Failed(
            CriticalNodeFailure::UserRegistryTerminated
        ))
    );
}
