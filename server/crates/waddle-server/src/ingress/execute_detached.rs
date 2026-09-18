//! Append one resource before atomically committing its progress and receipt.
use jid::FullJid;
use waddle_xmpp::ingress::{IngressEffectIntent, MessageKey};

use crate::{
    ingress_uow::{
        settle_recorded, CanonicalMessageRepository, DeliveryProgressRepository, IngressUnitOfWork,
        IngressUowError,
    },
    server::routes::interpret::{
        close_call_setup_from_outcome, deliver_direct_to_full_locally,
        deliver_direct_to_full_with_registered_remote, deliver_peer_to_full_with_registered_remote,
        effects::{
            delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
            EffectOutcome, ImmediateSink, SettledCompletion, SettledOutcome,
        },
        queue_processed_for_detached, Deps, DetachedQueueOutcome, FullJidDeliveryOutcome,
        SmIngressAppendContext,
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
    let no_call_setup = None;
    let (resources, call_setup) = match effect {
        ExternalDeliveryEffect::HostOwnedCopy { target, .. } => {
            (vec![target.clone()], &no_call_setup)
        }
        ExternalDeliveryEffect::QueueDetached {
            resources,
            call_setup,
            ..
        } => (resources.clone(), call_setup),
        ExternalDeliveryEffect::RouteToPeer {
            jid, call_setup, ..
        }
        | ExternalDeliveryEffect::RelayFullJid {
            target: jid,
            call_setup,
            ..
        } => (vec![jid.clone()], call_setup),
        _ => return EffectOutcome::Unavailable,
    };
    let external =
        crate::server::routes::interpret::effects::ExternalEffect::Delivery(effect.clone());
    let Some(progress) = decision.route_progress.iter().find(|progress| {
        (!matches!(effect, ExternalDeliveryEffect::RelayFullJid { .. }) || !progress.is_direct())
            && progress.matches(&external)
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
        // The recorded receipt, rather than today's stanza or audience, owns
        // this resource's append. Each resource gets an independent context.
        let mut resource_deps = immediate.clone();
        resource_deps.ingress_append_context = Some(SmIngressAppendContext {
            message_key: key,
            receipt: progress.receipt.clone(),
            received_at: progress.received_at,
        });
        let ResourceDelivery { outcome, certainty } =
            append_resource(&resource_deps, effect, resource).await;
        destinations.push((resource.clone(), outcome));
        if certainty == DeliveryCertainty::Uncertain {
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
    // Each effect completes its own resources; another effect may still own
    // the remaining fanout. Only `persisted` proves the aggregate receipt.
    if completion != SettledCompletion::Uncertain
        && !destinations.is_empty()
        && destinations.iter().all(|(_, outcome)| accepted(*outcome))
    {
        completion = SettledCompletion::Complete;
    }
    let delivery_outcome = fanout_outcome(&destinations);
    close_call_setup_from_outcome(call_setup.clone(), delivery_outcome);
    EffectOutcome::Settled(SettledOutcome {
        refusal: None,
        persisted,
        completion,
        detached: Some(destinations),
    })
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum DeliveryCertainty {
    Proven,
    Uncertain,
}

struct ResourceDelivery {
    outcome: FullJidDeliveryOutcome,
    certainty: DeliveryCertainty,
}

async fn append_resource(
    deps: &Deps<'_>,
    effect: &ExternalDeliveryEffect,
    resource: &FullJid,
) -> ResourceDelivery {
    let mut certainty = DeliveryCertainty::Proven;
    let outcome = match effect {
        ExternalDeliveryEffect::HostOwnedCopy { target, .. } if target == resource => {
            FullJidDeliveryOutcome::Delivered
        }
        ExternalDeliveryEffect::QueueDetached { stanza, .. } => {
            let outcomes = queue_processed_for_detached(
                deps,
                vec![resource.clone()],
                &std::collections::HashSet::new(),
                stanza,
            )
            .await;
            if outcomes
                .iter()
                .any(|(_, outcome)| *outcome == DetachedQueueOutcome::AppendFailed)
            {
                certainty = DeliveryCertainty::Uncertain;
            }
            if outcomes.contains(&(resource.clone(), DetachedQueueOutcome::Queued)) {
                FullJidDeliveryOutcome::QueuedDetached
            } else if deps.delivery_execution_context
                == crate::server::routes::interpret::DeliveryExecutionContext::MaintenanceRecovery
            {
                deliver_direct_to_full_locally(deps, resource, stanza)
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
        ExternalDeliveryEffect::RelayFullJid { .. } => {
            super::relay_copy::append(deps, effect).await
        }
        _ => FullJidDeliveryOutcome::Unavailable,
    };
    #[cfg(feature = "clustering")]
    if outcome == FullJidDeliveryOutcome::MaybeCommitted {
        certainty = DeliveryCertainty::Uncertain;
    }
    ResourceDelivery { outcome, certainty }
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
        settle_recorded(&mut tx, key, &[progress.settle_evidence()]).await?
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
