use super::{
    CriticalNodeFailure, NodeAdmission, NodeAdmissionError, NodeLifecycle, StartupServingTransition,
};

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
