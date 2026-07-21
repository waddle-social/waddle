use super::*;

mod delivery;
mod directed;
mod roster_state;
mod storage;

pub(super) use delivery::record_stanza_for_detached_available_resources_excluding;
pub(super) use delivery::try_route_presence_to_bare_remote;
pub use delivery::{
    broadcast_unavailable_for_terminated_session, TerminatedPresenceBroadcastOutcome,
};
pub(in crate::server::routes::websocket::handlers) use delivery::{
    send_current_presence_from_user_to_jid, send_current_presence_from_user_to_user,
    send_unavailable_presence_from_user_to_jid, send_unavailable_presence_from_user_to_user,
};
pub(super) use directed::handle_directed_presence;
pub(super) use roster_state::parse_subscription_state;
pub(super) use storage::{recipient_blocks_sender, roster_storage};

use delivery::{
    record_stanza_for_detached_available_resources,
    record_subscription_stanza_for_detached_resources_excluding, send_existing_subscription_ack,
    send_subscription_presence_side_effects, subscription_presence_recipients,
};
#[cfg(feature = "clustering")]
use roster_state::send_current_roster_push_to_local_resources;
use roster_state::update_subscription_roster_state;

#[cfg(feature = "clustering")]
fn user_actor_route_origin(
    user: &BareJid,
) -> crate::server::routes::interpret::OrderedRelayRouteOrigin {
    let entity = waddle_xmpp::ownership::Entity::new(
        waddle_xmpp::ownership::EntityType::UserActor,
        user.to_string(),
    );
    crate::server::routes::interpret::OrderedRelayRouteOrigin {
        kind: crate::server::routes::interpret::OrderedRelayRouteOriginKind::Entity(entity.clone()),
        sender_entity: entity,
        inbound_sequence: 0,
        handoff: None,
    }
}

#[cfg(not(feature = "clustering"))]
pub(super) async fn try_handle_remote_subscription_presence(
    _state: &WebSocketState,
    _request: &waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
    _ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) -> bool {
    false
}

#[cfg(feature = "clustering")]
pub(super) async fn try_handle_remote_subscription_presence(
    state: &WebSocketState,
    request: &waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) -> bool {
    let fallback_origin;
    let origin = match ordered_relay_origin {
        Some(origin) => origin,
        None => {
            fallback_origin = user_actor_route_origin(&request.from);
            &fallback_origin
        }
    };
    let Some(storage) = roster_storage(state).await else {
        return false;
    };
    let sender_had_presence_subscribers = match request.subscription_type {
        SubscriptionType::Unsubscribed => {
            match storage.get_roster_item(&request.from, &request.to).await {
                Ok(Some(row)) => matches!(
                    parse_subscription_state(&row.subscription),
                    Subscription::From | Subscription::Both
                ),
                Ok(None) => false,
                Err(error) => {
                    warn!(
                        error = %error,
                        from = %request.from,
                        to = %request.to,
                        "failed to read sender roster state before remote unsubscribed relay"
                    );
                    false
                }
            }
        }
        _ => false,
    };
    let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    else {
        return false;
    };
    let stanza = Stanza::Presence(build_subscription_presence(
        request.subscription_type,
        &request.from,
        &request.to,
        request.status.as_deref(),
        &request.payloads,
    ));
    let outcome = bridge
        .try_deliver_bare_jid_remote(&request.to, &stanza, origin)
        .await;
    debug!(
        from = %request.from,
        to = %request.to,
        kind = ?request.subscription_type,
        outcome = ?outcome,
        "remote subscription presence relay attempt completed"
    );
    let delivered = outcome.is_some_and(|outcome| outcome.suppresses_fallback());
    if !delivered {
        return false;
    }

    if matches!(
        request.subscription_type,
        SubscriptionType::Subscribed | SubscriptionType::Unsubscribed
    ) {
        state
            .deps
            .protocol
            .connection_registry
            .remove_pending_subscribe(&request.from, &request.to);
    }
    if let Err(error) =
        send_current_roster_push_to_local_resources(state, &storage, &request.from, &request.to)
            .await
    {
        warn!(
            error = %error,
            from = %request.from,
            to = %request.to,
            "failed to replay sender-local roster push after remote subscription relay"
        );
    }
    match request.subscription_type {
        SubscriptionType::Subscribed => {
            send_current_presence_from_user_to_user(
                state,
                &request.from,
                &request.to,
                Some(origin),
            )
            .await;
        }
        SubscriptionType::Unsubscribed if sender_had_presence_subscribers => {
            send_unavailable_presence_from_user_to_user(
                state,
                &request.from,
                &request.to,
                Some(origin),
            )
            .await;
        }
        SubscriptionType::Subscribe
        | SubscriptionType::Unsubscribe
        | SubscriptionType::Unsubscribed => {}
    }
    true
}

pub(super) async fn handle_subscription_presence(
    state: &WebSocketState,
    request: waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) {
    #[cfg(feature = "clustering")]
    let fallback_origin;
    #[cfg(feature = "clustering")]
    let ordered_relay_origin = match ordered_relay_origin {
        Some(origin) => Some(origin),
        None => {
            fallback_origin = user_actor_route_origin(&request.from);
            Some(&fallback_origin)
        }
    };

    if recipient_blocks_sender(state, &request.to, &request.from).await {
        debug!(from = %request.from, to = %request.to, "Dropping subscription presence blocked by recipient");
        return;
    }
    if request.subscription_type == SubscriptionType::Unsubscribe {
        state
            .deps
            .protocol
            .connection_registry
            .remove_pending_subscribe(&request.to, &request.from);
    }
    if matches!(
        request.subscription_type,
        SubscriptionType::Subscribed | SubscriptionType::Unsubscribed
    ) {
        state
            .deps
            .protocol
            .connection_registry
            .remove_pending_subscribe(&request.from, &request.to);
    }

    let Some(storage) = roster_storage(state).await else {
        return;
    };
    let update = match update_subscription_roster_state(
        state,
        &storage,
        &request,
        ordered_relay_origin,
    )
    .await
    {
        Ok(Some(update)) => update,
        Ok(None) => {
            debug!(from = %request.from, to = %request.to, kind = ?request.subscription_type, "Ignoring invalid subscription transition");
            return;
        }
        Err(error) => {
            warn!(error = %error, from = %request.from, to = %request.to, "Failed to update roster subscription state");
            return;
        }
    };

    // Roster pushes are now emitted inline by `update_subscription_roster_state`
    // (see PR #336 review on cross-user lock deadlock). Only the post-roster
    // presence side effects remain here.
    if update.auto_approve_subscribe {
        send_existing_subscription_ack(
            state,
            &request.to,
            &request.from,
            request.status.as_deref(),
            &request.payloads,
            ordered_relay_origin,
        )
        .await;
        send_current_presence_from_user_to_user(
            state,
            &request.to,
            &request.from,
            ordered_relay_origin,
        )
        .await;
        return;
    }

    if !update.forward_subscription_stanza {
        return;
    }

    let stanza = Stanza::Presence(build_subscription_presence(
        request.subscription_type,
        &request.from,
        &request.to,
        request.status.as_deref(),
        &request.payloads,
    ));
    if request.subscription_type == SubscriptionType::Subscribe {
        state
            .deps
            .protocol
            .connection_registry
            .queue_pending_subscription_stanza(&request.to, stanza.clone());
    }
    if request.subscription_type == SubscriptionType::Unsubscribed
        && update.send_unavailable_before_unsubscribed
    {
        send_unavailable_presence_from_user_to_user(
            state,
            &request.from,
            &request.to,
            ordered_relay_origin,
        )
        .await;
    }

    let routed_remote_after_local_update = request.subscription_type
        == SubscriptionType::Subscribed
        && try_route_presence_to_bare_remote(state, &request.to, &stanza, ordered_relay_origin)
            .await;

    if !routed_remote_after_local_update {
        let resources = subscription_presence_recipients(state, &request);
        let mut delivered = 0usize;
        let mut delivered_resources = Vec::new();
        if resources.is_empty() && request.subscription_type == SubscriptionType::Subscribe {
            let _recorded = record_stanza_for_detached_available_resources(
                state,
                &request.to,
                &stanza,
                "subscription presence",
            )
            .await;
            state
                .deps
                .protocol
                .connection_registry
                .queue_pending_subscription_stanza(&request.to, stanza);
        } else {
            for resource in &resources {
                if state
                    .deps
                    .protocol
                    .connection_registry
                    .send_to(resource, stanza.clone())
                    .await
                    .is_sent()
                {
                    delivered += 1;
                    delivered_resources.push(resource.clone());
                }
            }
            delivered += record_subscription_stanza_for_detached_resources_excluding(
                state,
                &request,
                &stanza,
                &delivered_resources,
            )
            .await;
            if delivered == 0 && request.subscription_type == SubscriptionType::Subscribe {
                state
                    .deps
                    .protocol
                    .connection_registry
                    .queue_pending_subscription_stanza(&request.to, stanza);
            }
        }
    }

    send_subscription_presence_side_effects(state, &request, ordered_relay_origin).await;
}
