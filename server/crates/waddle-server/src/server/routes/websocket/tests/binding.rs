//! The binding ledger of one physical connection: one new identity per bind,
//! per resume attempt and per fresh bind after a failed resume, and steps
//! that only ever apply to the exact current attempt.

use super::super::connection::{
    BindingLedger, BindingRefusal, BindingStage, ConnectionActorOperationId, InstallOutcome,
    PreparedBinding, RetirementCause, RetirementFence, RetirementRefusal,
};
use super::super::registration::{register_bound_connection_after_frame, RegistrationAfterFrame};
use super::{
    create_test_session, create_test_websocket_state, mpsc, ConnectionPhase, FullJid,
    OutboundStanza, WsConnState,
};
use waddle_xmpp::ownership::{BindingRef, ConnectionRef, NodeLeaseIdentity};

fn generation() -> ConnectionRef {
    ConnectionRef::fresh(NodeLeaseIdentity::local_process())
}

fn operation() -> ConnectionActorOperationId {
    ConnectionActorOperationId::fresh().expect("operation key")
}

fn prepare(ledger: &mut BindingLedger) -> PreparedBinding {
    ledger.prepare_next().expect("prepared attempt")
}

#[test]
fn a_binding_is_prepared_installed_and_confirmed_in_order() {
    let connection = generation();
    let mut ledger = BindingLedger::new(connection.clone());
    assert_eq!(ledger.current(), None);

    let PreparedBinding { attempt, displaced } = prepare(&mut ledger);
    assert_eq!(displaced, None);
    assert_eq!(attempt.connection_ref, connection);
    assert_eq!(attempt.binding_ref.connection_ref(), &connection);
    assert_eq!(
        ledger.current(),
        Some((&attempt.binding_ref, BindingStage::Prepared))
    );

    // A result cannot be acknowledged for a binding that is not published.
    assert_eq!(ledger.confirm(&attempt), Err(BindingRefusal::NotInstalled));

    assert_eq!(ledger.install(&attempt), Ok(InstallOutcome::Installed));
    assert_eq!(
        ledger.current(),
        Some((&attempt.binding_ref, BindingStage::Installed))
    );
    // A retried install reports the outcome it already had.
    assert_eq!(
        ledger.install(&attempt),
        Ok(InstallOutcome::AlreadyInstalled)
    );

    assert_eq!(ledger.confirm(&attempt), Ok(()));
    assert_eq!(ledger.confirm(&attempt), Ok(()));
    assert_eq!(
        ledger.current(),
        Some((&attempt.binding_ref, BindingStage::Confirmed))
    );
}

#[test]
fn a_replayed_prepare_returns_the_same_attempt_and_an_older_operation_is_refused() {
    let mut ledger = BindingLedger::new(generation());
    let older = operation();
    let newer = operation();

    let first = ledger.prepare(newer).expect("prepared");
    let replayed = ledger.prepare(newer).expect("replayed");
    assert_eq!(replayed.attempt, first.attempt);
    assert_eq!(replayed.displaced, None);

    assert_eq!(ledger.prepare(older), Err(BindingRefusal::StaleOperation));

    // Once installed, the same operation no longer mints or replays.
    ledger.install(&first.attempt).expect("installed");
    assert_eq!(ledger.prepare(newer), Err(BindingRefusal::StaleOperation));
}

#[test]
fn every_bind_resume_and_fresh_bind_after_a_failed_resume_mints_a_new_binding() {
    let mut ledger = BindingLedger::new(generation());

    // A resume attempt that fails before publication.
    let failed_resume = prepare(&mut ledger).attempt;
    assert_eq!(ledger.retire_prepared(&failed_resume.binding_ref), Ok(()));
    assert_eq!(ledger.current(), None);

    // The fresh bind that follows on the same socket.
    let fresh = prepare(&mut ledger).attempt;
    ledger.install(&fresh).expect("fresh bind installed");

    // A later successful resume of another session on a new attempt.
    assert!(ledger.retire(&fresh.binding_ref));
    let resumed = prepare(&mut ledger).attempt;

    let minted: Vec<&BindingRef> = vec![
        &failed_resume.binding_ref,
        &fresh.binding_ref,
        &resumed.binding_ref,
    ];
    for (index, binding) in minted.iter().enumerate() {
        for other in &minted[index + 1..] {
            assert_ne!(binding, other, "each attempt has its own identity");
        }
    }
    assert!(failed_resume.epoch < fresh.epoch && fresh.epoch < resumed.epoch);
}

#[test]
fn a_new_attempt_replaces_the_current_one_and_reports_what_it_displaced() {
    let mut ledger = BindingLedger::new(generation());
    let first = prepare(&mut ledger).attempt;
    ledger.install(&first).expect("installed");

    let second = prepare(&mut ledger);
    assert_eq!(second.displaced, Some(first.binding_ref.clone()));

    // Every step of the replaced attempt is refused and changes nothing.
    assert_eq!(ledger.install(&first), Err(BindingRefusal::NotCurrent));
    assert_eq!(ledger.confirm(&first), Err(BindingRefusal::NotCurrent));
    assert!(!ledger.retire(&first.binding_ref));
    assert_eq!(
        ledger.retire_prepared(&first.binding_ref),
        Err(BindingRefusal::NotCurrent)
    );
    assert_eq!(
        ledger.current(),
        Some((&second.attempt.binding_ref, BindingStage::Prepared))
    );
}

#[test]
fn compensation_retires_only_the_exact_unpublished_attempt() {
    let connection = generation();
    let mut ledger = BindingLedger::new(connection.clone());
    let attempt = prepare(&mut ledger).attempt;

    // A reference minted for the same socket by anything else is not it.
    let stranger = BindingRef::fresh(connection);
    assert_eq!(
        ledger.retire_prepared(&stranger),
        Err(BindingRefusal::NotCurrent)
    );
    let foreign = BindingRef::fresh(generation());
    assert_eq!(
        ledger.retire_prepared(&foreign),
        Err(BindingRefusal::WrongConnection)
    );
    assert_eq!(
        ledger.current(),
        Some((&attempt.binding_ref, BindingStage::Prepared))
    );

    assert_eq!(ledger.retire_prepared(&attempt.binding_ref), Ok(()));
    assert_eq!(ledger.current(), None);
    // The retired attempt cannot be installed afterwards.
    assert_eq!(ledger.install(&attempt), Err(BindingRefusal::NotCurrent));
}

#[test]
fn an_installed_binding_is_not_retired_as_compensation() {
    let mut ledger = BindingLedger::new(generation());
    let attempt = prepare(&mut ledger).attempt;
    ledger.install(&attempt).expect("installed");

    assert_eq!(
        ledger.retire_prepared(&attempt.binding_ref),
        Err(BindingRefusal::Installed)
    );
    assert_eq!(
        ledger.current(),
        Some((&attempt.binding_ref, BindingStage::Installed))
    );
    // The connection's own retirement clears it by exact reference.
    assert!(ledger.retire(&attempt.binding_ref));
    assert!(!ledger.retire(&attempt.binding_ref));
}

#[test]
fn an_attempt_of_another_connection_is_refused() {
    let mut ledger = BindingLedger::new(generation());
    let mut other = BindingLedger::new(generation());
    let foreign = prepare(&mut other).attempt;
    prepare(&mut ledger);

    assert_eq!(
        ledger.install(&foreign),
        Err(BindingRefusal::WrongConnection)
    );
    assert_eq!(
        ledger.confirm(&foreign),
        Err(BindingRefusal::WrongConnection)
    );
}

#[test]
fn a_binding_step_for_a_retired_generation_carries_the_retirement() {
    let connection = generation();
    let fence = RetirementFence::new(connection.clone());
    fence.retire(RetirementCause::Replaced);
    let refusal: BindingRefusal = fence.admit(&connection).expect_err("retired").into();
    assert_eq!(
        refusal,
        BindingRefusal::Retirement(RetirementRefusal::Retired(RetirementCause::Replaced))
    );
}

/// The registry publication of a bind uses the identity the connection's
/// ledger minted, and records it as installed once it is published.
#[tokio::test]
async fn a_registered_bind_installs_the_binding_its_ledger_minted() {
    let state = create_test_websocket_state().await;
    let jid: FullJid = "alice@example.com/ledger".parse().expect("JID");
    let mut conn = WsConnState::new();
    conn.phase = ConnectionPhase::ready(jid.clone(), false);
    conn.authenticated_session = Some(create_test_session(&state, "alice").await);
    let (tx, _outbound_rx) = mpsc::channel::<OutboundStanza>(4);
    let mut pending_tx = Some(tx);

    let registered = register_bound_connection_after_frame(
        state.as_ref(),
        "example.com",
        &mut conn,
        &mut pending_tx,
    )
    .await;
    assert!(matches!(registered, RegistrationAfterFrame::Registered(_)));

    let binding = conn.binding_ref.clone().expect("installed binding");
    assert_eq!(
        conn.bindings.current(),
        Some((&binding, BindingStage::Installed))
    );
    let owner = conn.registry_owner.expect("registry owner");
    let entry = state
        .deps
        .protocol
        .connection_registry
        .entry_if_owner(&jid, &owner)
        .expect("published entry");
    assert_eq!(entry.binding_ref(), Some(&binding));

    // The bind result reaching the wire acknowledges it.
    conn.confirm_installed_binding();
    assert_eq!(
        conn.bindings.current(),
        Some((&binding, BindingStage::Confirmed))
    );

    // Retiring the binding clears the ledger by the same exact reference.
    conn.retire_binding_ref();
    assert_eq!(conn.bindings.current(), None);
}
