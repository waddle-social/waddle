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

#[path = "execute_offline.rs"]
mod offline;
#[cfg(test)]
pub(crate) use offline::fail_before_offline_settlement;

#[path = "execute_archive.rs"]
mod archive;
#[path = "execute_recovery.rs"]
mod recovery;
#[cfg(test)]
pub(crate) use recovery::fail_after_recovery_update;

#[path = "execute_detached.rs"]
mod detached;
#[cfg(test)]
pub(crate) use detached::{FAIL_DELIVERY_PROGRESS_TX, STALL_DELIVERY_RESOURCE};

pub(super) fn owns(effect: &ExternalEffect, route_progress: &[RouteProgress]) -> bool {
    match effect {
        ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery { .. }) => true,
        ExternalEffect::Room(
            ExternalRoomEffect::ArchiveAfterPin { .. }
            | ExternalRoomEffect::NotificationCandidate { .. },
        ) => true,
        ExternalEffect::Delivery(ExternalDeliveryEffect::QueueDetached {
            bare,
            route_identity,
            ..
        }) => route_progress.iter().any(|progress| {
            &progress.recipient == bare && Some(&progress.route_identity) == route_identity.as_ref()
        }),
        ExternalEffect::Delivery(ExternalDeliveryEffect::RouteToPeer {
            jid,
            route_identity,
            ..
        }) => route_progress.iter().any(|progress| {
            progress.recipient == jid.to_bare()
                && Some(&progress.route_identity) == route_identity.as_ref()
        }),
        _ => false,
    }
}

/// Specialized invitation routes always remain generic.
/// The caller wraps this entire future in its existing timeout_at(deadline).
pub(super) async fn execute_with_uow(
    uow: &IngressUnitOfWork,
    _db: &Database,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalEffect,
    deps: &Deps<'_>,
    _deadline: tokio::time::Instant,
) -> Option<EffectOutcome> {
    match effect {
        ExternalEffect::Delivery(
            delivery @ ExternalDeliveryEffect::QueueOfflineDelivery { .. },
        ) => Some(offline::execute(uow, decision, index, delivery, deps).await),
        ExternalEffect::Room(room @ ExternalRoomEffect::ArchiveAfterPin { .. }) => {
            Some(archive::execute(uow, decision, index, room).await)
        }
        ExternalEffect::Room(room @ ExternalRoomEffect::NotificationCandidate { .. }) => {
            Some(recovery::execute(uow, decision, index, room).await)
        }
        ExternalEffect::Delivery(
            delivery @ (ExternalDeliveryEffect::QueueDetached { .. }
            | ExternalDeliveryEffect::RouteToPeer { .. }),
        ) if owns(effect, &decision.route_progress) => {
            Some(detached::execute(uow, decision, index, delivery, deps).await)
        }
        _ => None,
    }
}
