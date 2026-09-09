//! Append one resource before atomically committing its progress and receipt.
use jid::FullJid;
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey};

use crate::{
    ingress_uow::{
        settle_recorded, CanonicalMessageRepository, DeliveryProgressRepository, IngressUnitOfWork,
        IngressUowError,
    },
    server::routes::interpret::{
        close_call_setup_from_outcome, deliver_direct_to_full_with_registered_remote,
        deliver_peer_to_full_with_registered_remote,
        effects::{
            delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
            EffectOutcome, ImmediateSink, SettledCompletion, SettledOutcome,
        },
        queue_processed_for_detached, Deps, FullJidDeliveryOutcome,
    },
};

use super::super::{decision::IngressDecision, recorded::RouteProgress};

#[cfg(test)]
tokio::task_local! {
    pub(crate) static FAIL_DELIVERY_PROGRESS_TX: bool;
    pub(crate) static STALL_DELIVERY_RESOURCE: (FullJid, std::sync::Arc<std::sync::atomic::AtomicBool>);
}

pub(super) async fn execute(
    uow: &IngressUnitOfWork,
    decision: &IngressDecision,
    index: usize,
    effect: &ExternalDeliveryEffect,
    deps: &Deps<'_>,
) -> EffectOutcome {
    let (recipient, identity, resources, call_setup) = match effect {
        ExternalDeliveryEffect::QueueDetached {
            bare,
            route_identity,
            resources,
            call_setup,
            ..
        } => (bare.clone(), route_identity, resources.clone(), call_setup),
        ExternalDeliveryEffect::RouteToPeer {
            jid,
            route_identity,
            call_setup,
            ..
        } => (jid.to_bare(), route_identity, vec![jid.clone()], call_setup),
        _ => return EffectOutcome::Unavailable,
    };
    let Some(progress) = decision.route_progress.iter().find(|progress| {
        progress.recipient == recipient
            && Some(&progress.route_identity) == identity.as_ref()
            && decision.external_receipts[index].contains(&progress.receipt)
    }) else {
        return EffectOutcome::Unavailable;
    };
    let Some(key) = decision.message_key else {
        return EffectOutcome::Unavailable;
    };
    let mut immediate = deps.clone();
    immediate.effects = &ImmediateSink;
    let mut destinations = Vec::with_capacity(resources.len());
    let mut persisted = Vec::new();
    let mut completion = SettledCompletion::Incomplete;
    for resource in resources.iter().filter(|resource| {
        progress.fanout.contains(resource) && !progress.completed.contains(resource)
    }) {
        #[cfg(test)]
        if STALL_DELIVERY_RESOURCE
            .try_with(|(target, entered)| {
                if target == resource {
                    entered.store(true, std::sync::atomic::Ordering::SeqCst);
                    true
                } else {
                    false
                }
            })
            .unwrap_or(false)
        {
            std::future::pending::<()>().await;
        }
        let outcome = append_resource(&immediate, effect, resource).await;
        destinations.push((resource.clone(), outcome));
        #[cfg(feature = "clustering")]
        if outcome == FullJidDeliveryOutcome::MaybeCommitted {
            completion = SettledCompletion::Uncertain;
        }
        if accepted(outcome) {
            match record_resource(uow, key, progress, resource).await {
                Ok(settled) => {
                    if !settled.is_empty() {
                        persisted = settled;
                    }
                }
                Err(error) => {
                    tracing::warn!(%error, %resource, "delivery progress transaction failed after append");
                    completion = SettledCompletion::Uncertain;
                    // Do not append another resource after losing this one's progress.
                    break;
                }
            }
        }
    }
    if completion != SettledCompletion::Uncertain
        && !persisted.is_empty()
        && !destinations.is_empty()
        && destinations.iter().all(|(_, outcome)| accepted(*outcome))
    {
        completion = SettledCompletion::Complete;
    }
    let delivery_outcome = fanout_outcome(&destinations);
    close_call_setup_from_outcome(call_setup.clone(), delivery_outcome);
    EffectOutcome::Settled(SettledOutcome {
        persisted,
        completion,
        detached: Some(destinations),
    })
}

async fn append_resource(
    deps: &Deps<'_>,
    effect: &ExternalDeliveryEffect,
    resource: &FullJid,
) -> FullJidDeliveryOutcome {
    match effect {
        ExternalDeliveryEffect::QueueDetached { stanza, .. } => {
            let (queued, _) = queue_processed_for_detached(
                deps,
                vec![resource.clone()],
                &std::collections::HashSet::new(),
                stanza,
            )
            .await;
            if queued.contains(resource) {
                FullJidDeliveryOutcome::QueuedDetached
            } else {
                deliver_direct_to_full_with_registered_remote(deps, resource, stanza).await
            }
        }
        ExternalDeliveryEffect::RouteToPeer { stanza, kind, .. } => match kind {
            PeerDeliveryKind::RegistryFrame => {
                if deps
                    .connection_registry
                    .try_send_to(resource, *stanza.clone())
                    == waddle_xmpp::registry::BroadcastOutcome::Delivered
                {
                    FullJidDeliveryOutcome::Delivered
                } else {
                    FullJidDeliveryOutcome::Unavailable
                }
            }
            PeerDeliveryKind::PeerStanza => {
                deliver_peer_to_full_with_registered_remote(deps, resource, stanza).await
            }
            PeerDeliveryKind::DirectFrame => {
                deliver_direct_to_full_with_registered_remote(deps, resource, stanza).await
            }
        },
        _ => FullJidDeliveryOutcome::Unavailable,
    }
}

async fn record_resource(
    uow: &IngressUnitOfWork,
    key: MessageKey,
    progress: &RouteProgress,
    resource: &FullJid,
) -> Result<Vec<IngressEffectIntent>, IngressUowError> {
    let mut tx = uow
        .begin_with_timeouts(
            std::time::Duration::from_millis(100),
            std::time::Duration::from_millis(250),
        )
        .await?;
    if !CanonicalMessageRepository::lock(&mut tx, key).await? {
        return Err(IngressUowError::EffectIntentMessageMissing);
    }
    DeliveryProgressRepository::record(
        &mut tx,
        key,
        &progress.receipt,
        std::slice::from_ref(resource),
    )
    .await?;
    #[cfg(test)]
    if FAIL_DELIVERY_PROGRESS_TX
        .try_with(|fail| *fail)
        .unwrap_or(false)
    {
        return Err(IngressUowError::Timeout);
    }
    let completed = DeliveryProgressRepository::load(&mut tx, key, &progress.receipt).await?;
    let persisted = if progress
        .fanout
        .iter()
        .all(|target| completed.contains(target))
    {
        settle_recorded(
            &mut tx,
            key,
            &[IngressEffectIntent::RouteDirect {
                recipient: progress.recipient.clone(),
                fanout: progress.fanout.clone(),
                route_identity: progress.route_identity.clone(),
            }],
        )
        .await?
    } else {
        Vec::new()
    };
    tx.commit().await?;
    Ok(persisted)
}

fn accepted(outcome: FullJidDeliveryOutcome) -> bool {
    matches!(
        outcome,
        FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached
    )
}

fn fanout_outcome(destinations: &[(FullJid, FullJidDeliveryOutcome)]) -> FullJidDeliveryOutcome {
    if destinations.is_empty() {
        FullJidDeliveryOutcome::Unavailable
    } else if destinations.iter().all(|(_, outcome)| accepted(*outcome)) {
        if destinations
            .iter()
            .any(|(_, outcome)| *outcome == FullJidDeliveryOutcome::QueuedDetached)
        {
            FullJidDeliveryOutcome::QueuedDetached
        } else {
            FullJidDeliveryOutcome::Delivered
        }
    } else {
        FullJidDeliveryOutcome::Dropped
    }
}
