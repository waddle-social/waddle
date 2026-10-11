//! Deliver one progress-owned MUC relay copy before detached settlement records it.
use crate::server::routes::interpret::{
    deliver_full_jid_via_ordered_relay, deliver_peer_to_full_with_registered_remote,
    effects::delivery::ExternalDeliveryEffect, Deps, FullJidDeliveryOutcome,
};

pub(super) async fn append(
    deps: &Deps<'_>,
    effect: &ExternalDeliveryEffect,
) -> FullJidDeliveryOutcome {
    let ExternalDeliveryEffect::RelayFullJid {
        origin,
        target,
        stanza,
        call_setup,
        ..
    } = effect
    else {
        return FullJidDeliveryOutcome::Unavailable;
    };
    let mut relay_deps = deps.clone();
    relay_deps.ordered_relay_origin = origin.clone();
    match deliver_full_jid_via_ordered_relay(&relay_deps, target, stanza, call_setup.clone()).await
    {
        Some(outcome) => outcome,
        // A target that became local before the relay attempt still uses the
        // occupant's MUC append key established by the progress executor.
        None => deliver_peer_to_full_with_registered_remote(&relay_deps, target, stanza).await,
    }
}

/// Preserve the established remote wire path while its durable manifest
/// completion protocol remains unavailable. The canonical key is local-only,
/// and a legacy relay ACK cannot discharge either child or aggregate receipt.
pub(super) async fn append_sealed_legacy(
    deps: &Deps<'_>,
    effect: &ExternalDeliveryEffect,
    resource: &jid::FullJid,
) -> Option<FullJidDeliveryOutcome> {
    use crate::server::routes::interpret::{
        deliver_registered_remote_resource, DeliveryExecutionContext,
    };
    if deps.delivery_execution_context == DeliveryExecutionContext::MaintenanceRecovery {
        return None;
    }
    // A target that is local now keeps its frozen child and occupancy guards.
    if deps
        .connection_registry
        .get_entry(resource)
        .is_some_and(|entry| entry.is_locally_hosted())
    {
        return None;
    }
    let (stanza, kind) = match effect {
        ExternalDeliveryEffect::RelayFullJid { stanza, .. }
        | ExternalDeliveryEffect::RouteToPeer {
            stanza,
            kind: crate::server::routes::interpret::effects::delivery::PeerDeliveryKind::PeerStanza,
            ..
        } => (stanza, waddle_xmpp::registry::DeliveryKind::PeerStanza),
        ExternalDeliveryEffect::RouteToPeer {
            stanza,
            kind: crate::server::routes::interpret::effects::delivery::PeerDeliveryKind::DirectFrame,
            ..
        }
        | ExternalDeliveryEffect::QueueDetached { stanza, .. } => {
            (stanza, waddle_xmpp::registry::DeliveryKind::DirectFrame)
        }
        _ => return None,
    };
    let mut legacy = deps.clone();
    if let Some(context) = legacy.ingress_append_context.as_mut() {
        context.authority = crate::ingress::append_authority::AppendAuthority::LocalOnly;
    }
    if let ExternalDeliveryEffect::RelayFullJid {
        origin, call_setup, ..
    } = effect
    {
        legacy.ordered_relay_origin = origin.clone();
        if deliver_full_jid_via_ordered_relay(&legacy, resource, stanza, call_setup.clone())
            .await
            .is_some()
        {
            return Some(FullJidDeliveryOutcome::MaybeCommitted);
        }
    }
    deliver_registered_remote_resource(&legacy, resource, stanza, kind)
        .await
        .map(|_| FullJidDeliveryOutcome::MaybeCommitted)
}
