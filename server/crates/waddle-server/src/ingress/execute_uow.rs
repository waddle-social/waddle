//! Exact routing for effects that commit their work and receipts together.
use crate::{
    db::Database,
    ingress_uow::IngressUnitOfWork,
    server::routes::interpret::{
        effects::{
            delivery::ExternalDeliveryEffect, room::ExternalRoomEffect, EffectOutcome,
            ExternalEffect,
        },
        Deps,
    },
};

use super::{decision::IngressDecision, recorded::RouteProgress};

#[path = "execute_archive.rs"]
mod archive;
#[path = "execute_recovery.rs"]
mod recovery;
#[cfg(test)]
pub(crate) use recovery::fail_after_recovery_update;

pub(super) fn owns(effect: &ExternalEffect, _route_progress: &[RouteProgress]) -> bool {
    matches!(
        effect,
        ExternalEffect::Room(
            ExternalRoomEffect::ArchiveAfterPin { .. }
                | ExternalRoomEffect::NotificationCandidate { .. }
        )
    )
}

/// Later lanes add QueueDetached, RouteToPeer with recorded progress,
/// and QueueOfflineDelivery. Until then they remain
/// generic effects; specialized invitation routes always remain generic.
/// The caller wraps this entire future in its existing timeout_at(deadline).
pub(super) async fn execute_with_uow(
    uow: &IngressUnitOfWork,
    _db: &Database,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalEffect,
    _deps: &Deps<'_>,
    _deadline: tokio::time::Instant,
) -> Option<EffectOutcome> {
    match effect {
        ExternalEffect::Room(room @ ExternalRoomEffect::ArchiveAfterPin { .. }) => {
            Some(archive::execute(uow, decision, index, room).await)
        }
        ExternalEffect::Room(room @ ExternalRoomEffect::NotificationCandidate { .. }) => {
            Some(recovery::execute(uow, decision, index, room).await)
        }
        ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueDetached { .. }
            | ExternalDeliveryEffect::RouteToPeer { .. }
            | ExternalDeliveryEffect::QueueOfflineDelivery { .. },
        ) => None,
        _ => None,
    }
}
