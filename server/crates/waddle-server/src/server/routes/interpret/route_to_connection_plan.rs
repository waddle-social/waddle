pub(super) use super::super::effects::delivery::{ExternalDeliveryEffect, PeerDeliveryKind};
use super::*;

pub(super) fn record(deps: &Deps<'_>, effect: ExternalDeliveryEffect) {
    super::super::effects::delivery::record(deps, effect);
}

pub(super) fn queue_detached(deps: &Deps<'_>, resources: Vec<FullJid>, stanza: &Stanza) {
    let Some(first) = resources.first() else {
        return;
    };
    record(
        deps,
        ExternalDeliveryEffect::QueueDetached {
            call_setup: None,
            bare: first.to_bare(),
            resources,
            stanza: Box::new(stanza.clone()),
        },
    );
}

pub(super) async fn deliver_peer_to_live(
    deps: &Deps<'_>,
    target: &FullJid,
    stanza: &Stanza,
) -> FullJidDeliveryOutcome {
    if !deps.effects.is_planning() {
        return deliver_peer_to_live_only(deps.user_registry, target, stanza).await;
    }
    let resources = match deps.user_registry {
        Some(registry) => {
            waddle_xmpp::registry::get_resources_for_user(registry, &target.to_bare()).await
        }
        None => Vec::new(),
    };
    if !resources.contains(target) {
        return FullJidDeliveryOutcome::Unavailable;
    }
    record(
        deps,
        ExternalDeliveryEffect::RouteToPeer {
            jid: target.clone(),
            stanza: Box::new(stanza.clone()),
            kind: PeerDeliveryKind::PeerStanza,
            call_setup: None,
        },
    );
    FullJidDeliveryOutcome::Delivered
}

/// Read ownership only: bridge asks reserve sequence numbers and deliver, so
/// they must wait until the ingress transaction has committed.
pub(in crate::server::routes::interpret) async fn remote_owner(
    deps: &Deps<'_>,
    target: &BareJid,
) -> bool {
    #[cfg(feature = "clustering")]
    {
        let Some(state) = deps.web_socket_state else {
            return false;
        };
        let handles = &state.deps.app_state.clustering_claims;
        if handles.ordered_relay_delivery_bridge.is_none() || deps.ordered_relay_origin.is_none() {
            return false;
        }
        if deps.ordered_relay_origin.as_ref().is_some_and(|origin| {
            matches!(
                origin.kind,
                super::super::OrderedRelayRouteOriginKind::RemoteResource(_)
            )
        }) {
            return true;
        }
        let (Some(store), Some(identity)) = (&handles.claim_store, &handles.node_identity) else {
            return false;
        };
        let entity = waddle_xmpp::ownership::Entity::new(
            waddle_xmpp::ownership::EntityType::UserActor,
            target.to_string(),
        );
        matches!(store.current_claim(&entity).await, Ok(Some(claim))
            if claim.owner_lease_fresh && claim.owner != identity.current())
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (deps, target);
        false
    }
}

/// Bounce generation may revoke a Jingle token, so defer the whole obligation.
pub(super) fn bounce_nonexistent(deps: &Deps<'_>, stanza: &Stanza) -> Vec<Stanza> {
    if deps.effects.is_planning() {
        if deps.sfu.is_some() {
            if let Stanza::Iq(iq) = stanza {
                if let Some(rollback) =
                    waddle_xmpp::protocol::handlers::jingle::undeliverable_negotiation_rollback(iq)
                {
                    if let Some(jti) = rollback.minted_jti {
                        record(
                            deps,
                            ExternalDeliveryEffect::SfuRevokeToken {
                                call_id: rollback.call_id,
                                identity: rollback.identity,
                                jti,
                            },
                        );
                    }
                }
            }
        }
        for reply in bounce_for_nonexistent_account(stanza, None) {
            record(
                deps,
                ExternalDeliveryEffect::UndeliverableBounce {
                    reply: Box::new(reply),
                },
            );
        }
        Vec::new()
    } else {
        bounce_for_nonexistent_account(stanza, deps.sfu)
    }
}

/// Resolve the same live → detached → unavailable ladder without sending.
pub(super) async fn deliver_full(
    deps: &Deps<'_>,
    target: &FullJid,
    stanza: &Stanza,
    call_setup: Option<PendingCallSetupRoute>,
) -> FullJidDeliveryOutcome {
    if remote_owner(deps, &target.to_bare()).await {
        record(
            deps,
            ExternalDeliveryEffect::RelayFullJid {
                origin: deps.ordered_relay_origin.clone(),
                target: target.clone(),
                stanza: Box::new(stanza.clone()),
                call_setup,
            },
        );
        return FullJidDeliveryOutcome::Delivered;
    }
    let resources = match deps.user_registry {
        Some(registry) => {
            waddle_xmpp::registry::get_resources_for_user(registry, &target.to_bare()).await
        }
        None => Vec::new(),
    };
    if resources.contains(target) {
        record(
            deps,
            ExternalDeliveryEffect::RouteToPeer {
                jid: target.clone(),
                stanza: Box::new(stanza.clone()),
                kind: PeerDeliveryKind::PeerStanza,
                call_setup,
            },
        );
        return FullJidDeliveryOutcome::Delivered;
    }
    let detached = match deps.sm_session_registry {
        Some(sm) => sm
            .detached_resources_for_user(&target.to_bare())
            .await
            .unwrap_or_default(),
        None => Vec::new(),
    };
    if detached.contains(target) {
        record(
            deps,
            ExternalDeliveryEffect::QueueDetached {
                bare: target.to_bare(),
                resources: vec![target.clone()],
                stanza: Box::new(stanza.clone()),
                call_setup,
            },
        );
        return FullJidDeliveryOutcome::QueuedDetached;
    }
    close_call_setup_from_outcome(call_setup, FullJidDeliveryOutcome::Unavailable);
    FullJidDeliveryOutcome::Unavailable
}

pub(super) fn bounce_unavailable(deps: &Deps<'_>, stanza: &Stanza) -> Option<Stanza> {
    if deps.effects.is_planning() {
        if !matches!(stanza, Stanza::Iq(_)) {
            return None;
        }
        // Non-message stanzas use exactly the pure IQ bounce half. The shared
        // helper also records any minted-token compensation separately.
        bounce_nonexistent(deps, stanza);
        None
    } else {
        bounce_undeliverable_iq(stanza, deps.sfu)
    }
}
