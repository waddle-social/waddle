use super::regular::show_name;
use super::subscription::{parse_subscription_state, recipient_blocks_sender, roster_storage};
use super::{
    build_available_presence, build_subscription_presence, build_unavailable_presence, info, warn,
    BareJid, FullJid, Jid, Stanza, SubscriptionStateMachine, SubscriptionType, WebSocketState,
};
use crate::server::routes::interpret::OrderedRelayRouteOrigin;

pub(super) async fn handle_presence_probe(
    state: &WebSocketState,
    from: BareJid,
    from_full: Option<FullJid>,
    to: BareJid,
    to_full: Option<FullJid>,
    suppress_unavailable: bool,
    ordered_relay_origin: Option<&OrderedRelayRouteOrigin>,
) {
    if recipient_blocks_sender(state, &to, &from).await {
        info!(requester = %from, target = %to, "Blocked presence probe");
        return;
    }
    if from != to && recipient_blocks_sender(state, &from, &to).await {
        info!(requester = %from, target = %to, "Suppressed presence probe for contact blocked by requester");
        return;
    }
    if !presence_probe_authorized(state, &from, &to).await {
        info!(requester = %from, target = %to, "Unauthorized presence probe");
        if !suppress_unavailable {
            send_unsubscribed_probe_response(state, &to, &from, ordered_relay_origin).await;
        }
        return;
    }
    let mut available =
        crate::server::routes::websocket::handlers::account_reads::available_resources_with_presence(
            state, &to,
        )
        .await;
    let mut detached_available = match state
        .deps
        .protocol
        .sm_session_registry
        .available_detached_presence_states_for_user(&to)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(error = %error, target = %to, "Failed to list detached resources for presence probe");
            Vec::new()
        }
    };
    if let Some(to_full) = &to_full {
        available.retain(|(resource, _)| resource == to_full);
        detached_available.retain(|state| state.resource == *to_full);
    }
    detached_available.retain(|state| {
        !available
            .iter()
            .any(|(live_resource, _)| *live_resource == state.resource)
    });
    if available.is_empty() && detached_available.is_empty() {
        if suppress_unavailable {
            return;
        }
        let unavailable = Stanza::Presence(if let Some(to_full) = &to_full {
            let mut presence =
                xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
            presence.from = Some(Jid::from(to_full.clone()));
            presence.to = Some(match &from_full {
                Some(full) => Jid::from(full.clone()),
                None => Jid::from(from.clone()),
            });
            presence
        } else {
            let mut presence = build_unavailable_presence(&to, &from);
            if let Some(full) = &from_full {
                presence.to = Some(Jid::from(full.clone()));
            }
            presence
        });
        deliver_probe_reply(
            state,
            &from,
            from_full.as_ref(),
            None,
            unavailable,
            ordered_relay_origin,
        )
        .await;
        return;
    }
    let requester_resources = if from_full.is_some() {
        None
    } else {
        Some(
            crate::server::routes::websocket::handlers::account_reads::resources(state, &from)
                .await,
        )
    };
    for detached in detached_available {
        let mut probe_response = build_available_presence(
            &detached.resource,
            &from,
            detached.show.as_ref().map(show_name),
            detached.status.as_deref(),
            detached.priority,
        );
        if let Some(full) = &from_full {
            probe_response.to = Some(Jid::from(full.clone()));
        }
        // Relay the detached resource's own stored extension payloads
        // (XEP-0115 caps, XEP-0319 idle, anything else) verbatim, exactly
        // like the live branch below (issue #1103).
        probe_response.payloads.extend(detached.payloads);
        deliver_probe_reply(
            state,
            &from,
            from_full.as_ref(),
            requester_resources.as_deref(),
            Stanza::Presence(probe_response),
            ordered_relay_origin,
        )
        .await;
    }
    for (resource, presence_state) in available {
        let mut probe_response = build_available_presence(
            &resource,
            &from,
            presence_state
                .as_ref()
                .and_then(|state| state.show.as_deref()),
            presence_state
                .as_ref()
                .and_then(|state| state.status.as_deref()),
            presence_state
                .as_ref()
                .map(|state| state.priority)
                .unwrap_or(0),
        );
        if let Some(full) = &from_full {
            probe_response.to = Some(Jid::from(full.clone()));
        }
        // Relay the resource's own stored extension payloads (XEP-0115 caps,
        // XEP-0319 idle, anything else) verbatim — never server-rebuilt ones
        // (issue #1101).
        if let Some(stored) = &presence_state {
            probe_response
                .payloads
                .extend(stored.payloads.iter().cloned());
        }
        deliver_probe_reply(
            state,
            &from,
            from_full.as_ref(),
            requester_resources.as_deref(),
            Stanza::Presence(probe_response),
            ordered_relay_origin,
        )
        .await;
    }
}

async fn deliver_probe_reply(
    state: &WebSocketState,
    from: &BareJid,
    from_full: Option<&FullJid>,
    requester_resources: Option<&[FullJid]>,
    presence: Stanza,
    ordered_relay_origin: Option<&OrderedRelayRouteOrigin>,
) {
    if let Some(target_full) = from_full {
        if super::subscription::try_route_presence_to_full_remote(
            state,
            target_full,
            &presence,
            ordered_relay_origin,
        )
        .await
        {
            return;
        }
        let _ = state
            .deps
            .protocol
            .connection_registry
            .try_send_to(target_full, presence);
        return;
    }
    if super::subscription::try_route_presence_to_bare_remote(
        state,
        from,
        &presence,
        ordered_relay_origin,
    )
    .await
    {
        return;
    }
    let resources;
    let targets = match requester_resources {
        Some(slice) => slice,
        None => {
            resources =
                crate::server::routes::websocket::handlers::account_reads::resources(state, from)
                    .await;
            &resources
        }
    };
    for requester_resource in targets {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .try_send_to(requester_resource, presence.clone());
    }
}

async fn send_unsubscribed_probe_response(
    state: &WebSocketState,
    from: &BareJid,
    to: &BareJid,
    ordered_relay_origin: Option<&OrderedRelayRouteOrigin>,
) {
    let stanza = Stanza::Presence(build_subscription_presence(
        SubscriptionType::Unsubscribed,
        from,
        to,
        None,
        &[],
    ));
    if super::subscription::try_route_presence_to_bare_remote(
        state,
        to,
        &stanza,
        ordered_relay_origin,
    )
    .await
    {
        return;
    }
    for resource in
        crate::server::routes::websocket::handlers::account_reads::resources(state, to).await
    {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource, stanza.clone())
            .await;
    }
}

async fn presence_probe_authorized(state: &WebSocketState, from: &BareJid, to: &BareJid) -> bool {
    if from == to {
        return true;
    }
    let Some(storage) = roster_storage(state).await else {
        return false;
    };
    match storage.get_roster_item(from, to).await {
        Ok(Some(row)) => SubscriptionStateMachine::should_receive_presence(
            parse_subscription_state(&row.subscription),
        ),
        Ok(None) => false,
        Err(error) => {
            warn!(error = %error, requester = %from, target = %to, "Failed to authorize presence probe");
            false
        }
    }
}
