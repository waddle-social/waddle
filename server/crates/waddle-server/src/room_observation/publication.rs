use super::ObservationRuntimeError;
use crate::{
    ingress::{
        nested::{NestedContinuation, NestedOutcome},
        IngressDecisionClass,
    },
    ingress_uow::RoomObservationRepository,
    server::routes::{
        interpret::plan_room_result,
        websocket::{interpret_loop::build_interpret_deps, WebSocketState},
    },
};
use std::sync::Arc;
use waddle_extensions::RoomObservationSubscription;

pub(super) async fn publish_pending(
    state: &Arc<WebSocketState>,
    subscription: &RoomObservationSubscription,
) -> Result<(), ObservationRuntimeError> {
    let authority = &state.deps.protocol.ingress;
    let mut tx = authority.observation_transaction().await?;
    let publication = RoomObservationRepository::publication(&mut tx, subscription).await?;
    tx.commit().await?;
    let Some(publication) = publication else {
        return Ok(());
    };
    let observed_at = publication.source.observed_at.clone();
    let deps = build_interpret_deps(state, None);
    let submission = plan_room_result(&deps, publication).await?;
    let continuation = NestedContinuation::new(state.clone(), None, submission.sender.clone());
    let operation = authority
        .try_begin_nested()
        .map_err(|_| ObservationRuntimeError::AdmissionDeferred)?;
    match operation
        .commit_and_continue(submission, continuation)
        .await
    {
        NestedOutcome::Committed { .. } => {
            super::telemetry::published(&observed_at);
            Ok(())
        }
        NestedOutcome::Refused(crate::ingress::nested::NestedRefusal::Decision(
            IngressDecisionClass::PrincipalMissing,
        )) => Ok(()),
        NestedOutcome::Refused(_) => Err(ObservationRuntimeError::AdmissionDeferred),
    }
}
