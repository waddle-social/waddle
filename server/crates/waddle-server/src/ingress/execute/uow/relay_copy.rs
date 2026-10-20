//! Deliver one progress-owned MUC relay copy before detached settlement records it.
use crate::server::routes::interpret::{
    deliver_full_jid_via_ordered_relay, deliver_peer_to_full_with_registered_remote,
    effects::delivery::ExternalDeliveryEffect, Deps, FullJidDeliveryOutcome,
};

pub(super) enum LegacyRemoteAttempt {
    Wire(FullJidDeliveryOutcome),
    Superseded(Vec<waddle_xmpp::ingress::IngressEffectIntent>),
}

/// A pending P2 child cannot act as the completion barrier for the established
/// live remote wire. Its ordered channel owns enqueue FIFO; this does not prove
/// canonical archive ordering or authorize any durable manifest settlement.
pub(super) async fn attempt_before_readiness(
    uow: &crate::ingress::uow::IngressUnitOfWork,
    decision: &super::IngressDecision,
    index: usize,
    effect: &ExternalDeliveryEffect,
    resource: &jid::FullJid,
    deps: &Deps<'_>,
) -> Result<Option<LegacyRemoteAttempt>, crate::ingress::uow::IngressUowError> {
    use crate::server::routes::interpret::{
        effects::ExternalEffect, DeliveryExecutionContext, SmIngressAppendContext,
    };
    if deps.delivery_execution_context != DeliveryExecutionContext::Live
        || deps
            .connection_registry
            .get_entry(resource)
            .is_some_and(|entry| entry.is_locally_hosted())
    {
        return Ok(None);
    }
    let external = ExternalEffect::Delivery(effect.clone());
    let Some(progress) = decision.route_progress.iter().find(|progress| {
        progress.matches(&external)
            && progress.fanout.contains(resource)
            && !progress.completed.contains(resource)
            && decision.external_receipts[index].contains(&progress.receipt)
    }) else {
        return Ok(None);
    };
    let Some(key) = decision.message_key else {
        return Ok(None);
    };
    let stanza = match effect {
        ExternalDeliveryEffect::RouteToPeer { stanza, .. }
        | ExternalDeliveryEffect::RelayFullJid { stanza, .. }
        | ExternalDeliveryEffect::QueueDetached { stanza, .. } => stanza,
        _ => return Ok(None),
    };
    let context = SmIngressAppendContext {
        message_key: key,
        receipt: progress.receipt.clone(),
        received_at: progress.received_at,
        archive_positions: crate::ingress::archive_dispatch::positions(uow, key, &progress.receipt)
            .await?,
        dispatch_stream: None,
        authority: crate::ingress::append_authority::AppendAuthority::Verified,
    };
    let Some(manifest) =
        crate::ingress::room_manifest::replay::load(uow, &context, resource, stanza).await?
    else {
        return Ok(None);
    };
    let stop = deps.ingress_delivery_stop.clone().or_else(|| {
        deps.web_socket_state
            .map(|state| state.deps.protocol.ingress.delivery_stop_token())
    });
    if crate::ingress::live_delivery::live_delivery_status(uow, stop.as_ref(), &context, resource)
        .await?
        .is_some()
    {
        return Ok(None);
    }
    if let Some(settled) = crate::ingress::room_manifest::replay::settle_superseded(
        uow,
        &manifest,
        deps.room_registry,
        &context,
        resource,
    )
    .await?
    {
        // A proven replacement resolves the old logical child, never a send.
        // This check must precede the legacy wire bypass of archive readiness.
        return Ok(Some(LegacyRemoteAttempt::Superseded(settled)));
    }
    let mut live = deps.clone();
    live.ingress_append_context = Some(context);
    live.ingress_delivery_uow = Some(uow.clone());
    live.ingress_delivery_stop = stop;
    Ok(append_sealed_legacy(&live, effect, resource)
        .await
        .map(LegacyRemoteAttempt::Wire))
}

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
