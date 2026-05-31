use super::*;

mod delivery;
mod directed;
mod roster_state;
mod storage;

pub use delivery::broadcast_unavailable_for_expired_detached_session;
pub(super) use delivery::record_stanza_for_detached_available_resources_excluding;
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
use roster_state::update_subscription_roster_state;

pub(super) async fn handle_subscription_presence(
    state: &WebSocketState,
    request: waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
) {
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
    let update = match update_subscription_roster_state(state, &storage, &request).await {
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
        )
        .await;
        send_current_presence_from_user_to_user(state, &request.to, &request.from).await;
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
        send_unavailable_presence_from_user_to_user(state, &request.from, &request.to).await;
    }

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

    send_subscription_presence_side_effects(state, &request).await;
}
