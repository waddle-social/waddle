//! Execute delivery obligations through the existing immediate operations.
use super::super::{route_to_connection, routing, Deps, FullJidDeliveryOutcome};
use super::{
    delivery::{ExternalDeliveryEffect, PeerDeliveryKind},
    EffectOutcome, ImmediateSink,
};

pub(crate) async fn execute(effect: ExternalDeliveryEffect, deps: &Deps<'_>) -> EffectOutcome {
    let mut immediate = deps.clone();
    immediate.effects = &ImmediateSink;
    match effect {
        ExternalDeliveryEffect::UndeliverableBounce { reply } => {
            EffectOutcome::Frames(vec![*reply])
        }
        ExternalDeliveryEffect::SfuRevokeToken {
            call_id,
            identity,
            jti,
        } => {
            if let Some(sfu) = immediate.sfu {
                sfu.revoke_issued_token(&call_id, &identity, &jti);
            }
            EffectOutcome::Completed
        }
        ExternalDeliveryEffect::RouteToPeer {
            route_identity: _,
            jid,
            stanza,
            kind,
            call_setup,
        } => {
            let outcome = match kind {
                PeerDeliveryKind::RegistryFrame => {
                    if immediate.connection_registry.try_send_to(&jid, *stanza)
                        == waddle_xmpp::registry::BroadcastOutcome::Delivered
                    {
                        FullJidDeliveryOutcome::Delivered
                    } else {
                        FullJidDeliveryOutcome::Unavailable
                    }
                }
                PeerDeliveryKind::PeerStanza => {
                    route_to_connection::deliver_peer_to_full_with_registered_remote(
                        &immediate, &jid, &stanza,
                    )
                    .await
                }
                PeerDeliveryKind::DirectFrame => {
                    route_to_connection::deliver_direct_to_full_with_registered_remote(
                        &immediate, &jid, &stanza,
                    )
                    .await
                }
            };
            routing::close_call_setup_from_outcome(call_setup, outcome);
            EffectOutcome::Delivery(outcome)
        }
        ExternalDeliveryEffect::QueueDetached {
            resources,
            stanza,
            call_setup,
            ..
        } => {
            let outcome =
                queue_detached_without_direct_progress(&immediate, resources, &stanza).await;
            routing::close_call_setup_from_outcome(call_setup, outcome);
            EffectOutcome::Delivery(outcome)
        }
        ExternalDeliveryEffect::RelayFullJid {
            route_identity: _,
            origin,
            target,
            stanza,
            call_setup,
        } => {
            immediate.ordered_relay_origin = origin;
            let outcome = route_to_connection::deliver_full_jid_via_ordered_relay(
                &immediate,
                &target,
                &stanza,
                call_setup.clone(),
            )
            .await;
            EffectOutcome::Delivery(
                finish_full_jid_relay(outcome, &immediate, &target, &stanza, call_setup).await,
            )
        }
        ExternalDeliveryEffect::RelayBareJid {
            origin,
            target,
            stanza,
        } => {
            immediate.ordered_relay_origin = origin;
            EffectOutcome::Delivery(
                route_to_connection::deliver_bare_jid_via_ordered_relay(
                    &immediate, &target, &stanza,
                )
                .await
                .unwrap_or(FullJidDeliveryOutcome::Unavailable),
            )
        }
        ExternalDeliveryEffect::RelayCarbons {
            origin,
            owner,
            exclude,
            message,
            kind,
        } => {
            immediate.ordered_relay_origin = origin;
            super::super::carbons::relay_carbons_only(&immediate, &owner, &message, kind, &exclude)
                .await
                .unwrap_or(EffectOutcome::Unavailable)
        }
        ExternalDeliveryEffect::Carbons {
            owner,
            recipient,
            exclude: _,
            message,
            kind,
        } => EffectOutcome::Delivery(
            super::super::carbons::send_carbon_to_resource(
                &immediate, &owner, &recipient, &message, kind,
            )
            .await,
        ),
        ExternalDeliveryEffect::QueueOfflineDelivery {
            prepared_notification,
            row,
            original_message,
        } => {
            let confirmed = super::super::offline_delivery::apply_offline_delivery_row(
                &immediate,
                row,
                original_message,
                Some(prepared_notification),
            )
            .await;
            EffectOutcome::ConfirmedIntents(confirmed)
        }
    }
}

/// Non-direct obligations (notably MUC occupant delivery) retain their generic
/// completion path. Direct obligations are intercepted by the ingress arm.
async fn queue_detached_without_direct_progress(
    deps: &Deps<'_>,
    resources: Vec<jid::FullJid>,
    stanza: &waddle_xmpp::Stanza,
) -> FullJidDeliveryOutcome {
    let mut outcomes = Vec::with_capacity(resources.len());
    for resource in resources {
        let (queued, _) = route_to_connection::queue_processed_for_detached(
            deps,
            vec![resource.clone()],
            &std::collections::HashSet::new(),
            stanza,
        )
        .await;
        outcomes.push(if queued.contains(&resource) {
            FullJidDeliveryOutcome::QueuedDetached
        } else {
            route_to_connection::deliver_direct_to_full_with_registered_remote(
                deps, &resource, stanza,
            )
            .await
        });
    }
    if outcomes.is_empty() {
        FullJidDeliveryOutcome::Unavailable
    } else if outcomes.iter().all(|outcome| {
        matches!(
            outcome,
            FullJidDeliveryOutcome::Delivered | FullJidDeliveryOutcome::QueuedDetached
        )
    }) {
        if outcomes.contains(&FullJidDeliveryOutcome::QueuedDetached) {
            FullJidDeliveryOutcome::QueuedDetached
        } else {
            FullJidDeliveryOutcome::Delivered
        }
    } else {
        FullJidDeliveryOutcome::Dropped
    }
}

/// A handled relay owns its ticket, even when delivery was dropped or uncertain.
async fn finish_full_jid_relay(
    outcome: Option<FullJidDeliveryOutcome>,
    deps: &Deps<'_>,
    target: &jid::FullJid,
    stanza: &waddle_xmpp::Stanza,
    call_setup: Option<waddle_xmpp::telemetry::call::PendingCallSetupRoute>,
) -> FullJidDeliveryOutcome {
    if let Some(outcome) = outcome {
        return outcome;
    }
    let outcome =
        route_to_connection::deliver_peer_to_full_with_registered_remote(deps, target, stanza)
            .await;
    routing::close_call_setup_from_outcome(call_setup, outcome);
    outcome
}

#[cfg(test)]
#[path = "delivery_immediate_tests.rs"]
mod tests;
