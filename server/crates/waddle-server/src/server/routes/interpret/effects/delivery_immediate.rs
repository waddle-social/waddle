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
            let (queued, unqueued) = route_to_connection::queue_processed_for_detached(
                &immediate,
                resources,
                &std::collections::HashSet::new(),
                &stanza,
            )
            .await;
            let retried =
                route_to_connection::retry_unqueued_detached_as_live(&immediate, unqueued, &stanza)
                    .await;
            let outcome = if !queued.is_empty() {
                FullJidDeliveryOutcome::QueuedDetached
            } else if !retried.is_empty() {
                FullJidDeliveryOutcome::Delivered
            } else {
                FullJidDeliveryOutcome::Unavailable
            };
            routing::close_call_setup_from_outcome(call_setup, outcome);
            EffectOutcome::Delivery(outcome)
        }
        ExternalDeliveryEffect::RelayFullJid {
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
            if outcome.is_none() {
                routing::close_call_setup_from_outcome(
                    call_setup,
                    FullJidDeliveryOutcome::Unavailable,
                );
            }
            EffectOutcome::Delivery(outcome.unwrap_or(FullJidDeliveryOutcome::Unavailable))
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
            EffectOutcome::Delivery(
                super::super::carbons::relay_carbons_only(
                    &immediate, &owner, &message, kind, &exclude,
                )
                .await
                .unwrap_or(FullJidDeliveryOutcome::Unavailable),
            )
        }
        ExternalDeliveryEffect::Carbons {
            owner,
            exclude,
            message,
            kind,
        } => {
            super::super::carbons::send_carbons(
                immediate.connection_registry,
                &immediate,
                owner,
                message,
                kind,
                exclude,
            )
            .await;
            EffectOutcome::Completed
        }
        ExternalDeliveryEffect::QueueOfflineDelivery {
            prepared_notification,
            row,
            original_message,
        } => {
            super::super::offline_delivery::apply_offline_delivery_row(
                &immediate,
                row,
                original_message,
                Some(prepared_notification),
            )
            .await;
            EffectOutcome::Completed
        }
    }
}
