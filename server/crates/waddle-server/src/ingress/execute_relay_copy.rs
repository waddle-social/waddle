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
