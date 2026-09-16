//! Local progress must not depend on an unrelated remote occupant responding.
use super::{dependencies, IngressDecision};
use crate::server::routes::interpret::effects::{
    delivery::ExternalDeliveryEffect, ExternalEffect, PlannedEffect,
};

/// Keep the original decision indices and dependency graph. Only independent
/// local copies belonging to this remote copy's MUC receipt may pass it.
pub(super) fn local_before_remote(
    decision: &IngressDecision,
    planned: &[PlannedEffect],
    completed: &[Option<bool>],
    remote_index: usize,
) -> Option<usize> {
    let remote = &decision.external[remote_index];
    if !matches!(
        remote,
        ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid { .. })
    ) {
        return None;
    }
    planned.iter().enumerate().find_map(|(index, plan)| {
        if completed[index].is_some()
            || !matches!(
                &decision.external[index],
                ExternalEffect::Delivery(
                    ExternalDeliveryEffect::RouteToPeer { .. }
                        | ExternalDeliveryEffect::QueueDetached { .. }
                )
            )
            || dependencies::ready(&plan.dependencies, &decision.external, completed) != Some(true)
        {
            return None;
        }
        decision
            .route_progress
            .iter()
            .any(|progress| {
                !progress.is_direct()
                    && progress.matches(remote)
                    && progress.matches(&decision.external[index])
            })
            .then_some(index)
    })
}

#[cfg(test)]
#[path = "execute_muc_fanout_tests.rs"]
mod tests;
