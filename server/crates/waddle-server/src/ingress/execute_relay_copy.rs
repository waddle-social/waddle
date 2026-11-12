//! Deliver one progress-owned copy to the node that hosts its target.
use crate::server::routes::interpret::effects::delivery::ExternalDeliveryEffect;
use crate::server::routes::interpret::effects::delivery::PeerDeliveryKind;
use crate::server::routes::interpret::{
    deliver_full_jid_via_ordered_relay, deliver_peer_to_full_with_registered_remote, Deps,
    FullJidDeliveryOutcome,
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

/// Send a sealed room child to the node that hosts its target, on the wire
/// the effect names.
///
/// The copy carries its canonical obligation, so the hosting node claims the
/// one durable send attempt of this child before it writes to the socket and
/// proves the target's occupancy there. Its answer is therefore the child's
/// outcome: a repeat from this node, from a recovery pass or from another
/// node finds the completed attempt and writes nothing.
///
/// A copy rebuilt by recovery names no wire. It travels on the room's
/// ordered channel, as the owner's live fan-out does; only the node that
/// holds the room's claim can send on that channel.
pub(super) async fn append_sealed_remote(
    deps: &Deps<'_>,
    effect: &ExternalDeliveryEffect,
    resource: &jid::FullJid,
    room: &jid::BareJid,
) -> FullJidDeliveryOutcome {
    use crate::server::routes::interpret::deliver_registered_remote_resource;
    let (stanza, kind) = match effect {
        ExternalDeliveryEffect::RelayFullJid { stanza, .. }
        | ExternalDeliveryEffect::RouteToPeer {
            stanza,
            kind: PeerDeliveryKind::PeerStanza,
            ..
        } => (stanza, waddle_xmpp::registry::DeliveryKind::PeerStanza),
        ExternalDeliveryEffect::RouteToPeer {
            stanza,
            kind: PeerDeliveryKind::DirectFrame,
            ..
        }
        | ExternalDeliveryEffect::QueueDetached { stanza, .. } => {
            (stanza, waddle_xmpp::registry::DeliveryKind::DirectFrame)
        }
        _ => return FullJidDeliveryOutcome::Unavailable,
    };
    let (origin, call_setup) = match effect {
        ExternalDeliveryEffect::RelayFullJid {
            origin, call_setup, ..
        } => (origin.clone(), call_setup.clone()),
        _ => (None, None),
    };
    #[cfg(feature = "clustering")]
    let origin = origin
        .or_else(|| Some(crate::server::routes::interpret::OrderedRelayRouteOrigin::room(room)));
    #[cfg(not(feature = "clustering"))]
    let _ = room;
    let mut relay = deps.clone();
    relay.ordered_relay_origin = origin;
    if let Some(outcome) =
        deliver_full_jid_via_ordered_relay(&relay, resource, stanza, call_setup).await
    {
        return outcome;
    }
    deliver_registered_remote_resource(deps, resource, stanza, kind)
        .await
        .unwrap_or(FullJidDeliveryOutcome::Unavailable)
}
