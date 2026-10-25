//! Exact routing for effects that commit their work and receipts together.
use crate::db::Database;
use crate::ingress_uow::CarbonReceiptRepository;
use crate::ingress_uow::IngressUnitOfWork;
use crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect;
use crate::server::routes::interpret::effects::direct::ExternalDirectEffect;
use crate::server::routes::interpret::effects::room::ExternalRoomEffect;
use crate::server::routes::interpret::effects::SettledCompletion;
use crate::server::routes::interpret::effects::SettledOutcome;
use crate::server::routes::interpret::effects::{EffectOutcome, ExternalEffect};
use crate::server::routes::interpret::Deps;

use super::decision::IngressDecision;
use super::recorded::RouteProgress;

#[path = "execute_offline.rs"]
mod offline;
#[cfg(test)]
pub(crate) use offline::{fail_before_offline_settlement, retry_before_offline_settlement};

#[path = "execute_projection.rs"]
mod projection;

#[path = "execute_archive.rs"]
mod archive;
#[path = "execute_recovery.rs"]
mod recovery;
#[cfg(test)]
pub(crate) use recovery::fail_after_recovery_update;

#[path = "execute_relay_copy.rs"]
mod relay_copy;

pub(super) async fn execute_legacy_remote(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalEffect,
    deps: &Deps<'_>,
) -> Option<EffectOutcome> {
    let ExternalEffect::Delivery(
        delivery @ (ExternalDeliveryEffect::RelayFullJid { target, .. }
        | ExternalDeliveryEffect::RouteToPeer { jid: target, .. }),
    ) = effect
    else {
        return None;
    };
    match relay_copy::attempt_before_readiness(uow, decision, index, delivery, target, deps).await {
        Ok(Some(relay_copy::LegacyRemoteAttempt::Wire(outcome))) => {
            Some(EffectOutcome::Settled(SettledOutcome {
                refusal: None,
                persisted: Vec::new(),
                completion: SettledCompletion::Uncertain,
                detached: Some(vec![(target.clone(), outcome)]),
            }))
        }
        Ok(Some(relay_copy::LegacyRemoteAttempt::Superseded(persisted))) => {
            Some(EffectOutcome::Settled(SettledOutcome {
                refusal: None,
                persisted,
                completion: SettledCompletion::Complete,
                detached: None,
            }))
        }
        Ok(None) => None,
        Err(error) => {
            tracing::debug!(%error, "live legacy remote child authority unavailable");
            Some(EffectOutcome::Unavailable)
        }
    }
}

#[path = "execute_invite.rs"]
mod invite;
#[cfg(test)]
pub(crate) use invite::PAUSE_BEFORE_INVITATION_SETTLEMENT;

#[path = "execute_ambiguous_offline.rs"]
mod ambiguous_offline;
pub(crate) use ambiguous_offline::pending_id as ambiguous_offline_pending_id;

#[path = "execute_detached.rs"]
mod detached;
pub(in crate::ingress) use detached::record_delivery_progress;
pub(in crate::ingress) use detached::record_policy_progress;
#[cfg(test)]
pub(crate) use detached::CONTEND_DELIVERY_PROGRESS_ONCE;
#[cfg(test)]
pub(crate) use detached::{
    CANCEL_STALLED_DELIVERY, FAIL_DELIVERY_PROGRESS_TX, STALL_DELIVERY_RESOURCE,
};

pub(super) fn owns(effect: &ExternalEffect, route_progress: &[RouteProgress]) -> bool {
    match effect {
        ExternalEffect::Frame(stanza) => {
            matches!(stanza.as_ref(), waddle_xmpp::Stanza::Message(message)
            if message.type_ == xmpp_parsers::message::MessageType::Groupchat
                && waddle_xmpp::xep::xep0334::should_skip_storage(message))
        }
        ExternalEffect::Direct(
            ExternalDirectEffect::NotificationActivity { .. }
            | ExternalDirectEffect::LinkPreviewRefs { .. }
            | ExternalDirectEffect::ClearLinkPreviewRefs { .. },
        ) => true,
        ExternalEffect::Delivery(ExternalDeliveryEffect::QueueOfflineDelivery { .. }) => true,
        ExternalEffect::Room(
            ExternalRoomEffect::ArchiveAfterPin { .. }
            | ExternalRoomEffect::NotificationCandidate { .. },
        ) => true,
        ExternalEffect::Delivery(ExternalDeliveryEffect::RelayFullJid { .. }) => {
            route_progress.iter().any(|progress| {
                !progress.is_direct()
                    && progress.matches(effect)
                    && !progress.remaining(effect).is_empty()
            })
        }
        ExternalEffect::Delivery(
            ExternalDeliveryEffect::HostOwnedCopy { .. }
            | ExternalDeliveryEffect::QueueDetached { .. }
            | ExternalDeliveryEffect::RouteToPeer { .. },
        ) => route_progress.iter().any(|progress| {
            progress.matches(effect)
                && (progress.is_direct() || !progress.remaining(effect).is_empty())
        }),
        _ => false,
    }
}

/// Specialized invitations retain their generic receipt mapping, but their
/// mutually exclusive live and pending sinks share the canonical transaction.
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
    if matches!(
        effect,
        ExternalEffect::Direct(ExternalDirectEffect::PushInboxUpdate { .. })
    ) {
        match projection::execute_sealed_inbox_push(uow, decision, index, effect, deps).await {
            Ok(Some(outcome)) => return Some(outcome),
            Ok(None) => {}
            Err(error) => {
                tracing::debug!(%error, "sealed inbox refresh authority unavailable");
                return Some(EffectOutcome::Unavailable);
            }
        }
    }
    if let ExternalEffect::Frame(stanza) = effect {
        if owns(effect, &decision.route_progress) {
            return match super::room_manifest_replay::release_transient_reflection(
                uow, decision, index, stanza, deps,
            )
            .await
            {
                Ok(Some(frame)) => Some(EffectOutcome::Frames(vec![frame])),
                Ok(None) => None,
                Err(error) => {
                    tracing::debug!(%error, "transient room reflection authority unavailable");
                    Some(EffectOutcome::Unavailable)
                }
            };
        }
    }
    // A sealed reflection uses the ordinary direct child, including when its
    // captured plan selected relay delivery. Legacy relays keep their old arm.
    if let ExternalEffect::Delivery(
        delivery @ ExternalDeliveryEffect::RelayFullJid { stanza, .. },
    ) = effect
    {
        if matches!(stanza.as_ref(), waddle_xmpp::Stanza::Message(message)
            if message.type_ == xmpp_parsers::message::MessageType::Groupchat)
            && !owns(effect, &decision.route_progress)
            && decision.route_progress.iter().any(|progress| {
                progress.is_direct()
                    && progress.matches(effect)
                    && decision.external_receipts[index].contains(&progress.receipt)
            })
        {
            let Some(key) = decision.message_key else {
                return Some(EffectOutcome::Unavailable);
            };
            let (envelope, _) =
                match CarbonReceiptRepository::load_authority(uow.database(), key).await {
                    Ok(authority) => authority,
                    Err(_) => return Some(EffectOutcome::Unavailable),
                };
            if envelope.room_manifest().is_some() {
                return Some(detached::execute(uow, decision, index, delivery, deps).await);
            }
        }
    }
    match effect {
        ExternalEffect::Direct(
            ExternalDirectEffect::NotificationActivity { .. }
            | ExternalDirectEffect::LinkPreviewRefs { .. }
            | ExternalDirectEffect::ClearLinkPreviewRefs { .. },
        ) => Some(projection::execute(uow, decision, index, effect).await),
        ExternalEffect::RouteToPeer(route) | ExternalEffect::QueueOfflineDelivery(route) => {
            Some(Box::pin(invite::execute(uow, decision, index, route, deps)).await)
        }
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
            delivery @ (ExternalDeliveryEffect::HostOwnedCopy { .. }
            | ExternalDeliveryEffect::QueueDetached { .. }
            | ExternalDeliveryEffect::RouteToPeer { .. }
            | ExternalDeliveryEffect::RelayFullJid { .. }),
        ) if owns(effect, &decision.route_progress) => {
            Some(detached::execute(uow, decision, index, delivery, deps).await)
        }
        _ => None,
    }
}
