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

pub(super) fn owns(effect: &ExternalEffect, _route_progress: &[RouteProgress]) -> bool {
    matches!(
        effect,
        ExternalEffect::Room(ExternalRoomEffect::ArchiveAfterPin { .. })
    )
}

/// Later lanes add QueueDetached, RouteToPeer with recorded progress,
/// QueueOfflineDelivery, and NotificationCandidate. Until then they remain
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
        ExternalEffect::Delivery(
            ExternalDeliveryEffect::QueueDetached { .. }
            | ExternalDeliveryEffect::RouteToPeer { .. }
            | ExternalDeliveryEffect::QueueOfflineDelivery { .. },
        )
        | ExternalEffect::Room(ExternalRoomEffect::NotificationCandidate { .. }) => None,
        _ => None,
    }
}
