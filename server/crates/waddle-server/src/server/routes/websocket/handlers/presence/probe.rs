use super::regular::show_name;
use super::subscription::{parse_subscription_state, recipient_blocks_sender, roster_storage};
use super::*;

pub(super) async fn handle_presence_probe(
    state: &WebSocketState,
    from: BareJid,
    to: BareJid,
    to_full: Option<FullJid>,
) {
    if recipient_blocks_sender(state, &to, &from).await {
        info!(requester = %from, target = %to, "Blocked presence probe");
        return;
    }
    if !presence_probe_authorized(state, &from, &to).await {
        info!(requester = %from, target = %to, "Unauthorized presence probe");
        send_unsubscribed_probe_response(state, &to, &from).await;
        return;
    }
    let mut available = state
        .deps
        .protocol
        .connection_registry
        .get_available_resources_for_user(&to);
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
        let unavailable = Stanza::Presence(if let Some(to_full) = &to_full {
            let mut presence =
                xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
            presence.from = Some(Jid::from(to_full.clone()));
            presence.to = Some(Jid::from(from.clone()));
            presence
        } else {
            build_unavailable_presence(&to, &from)
        });
        for resource in
            waddle_xmpp::registry::get_resources_for_user(&state.deps.protocol.user_registry, &from)
                .await
        {
            let _ = state
                .deps
                .protocol
                .connection_registry
                .send_to(&resource, unavailable.clone())
                .await;
        }
        return;
    }
    let requester_resources =
        waddle_xmpp::registry::get_resources_for_user(&state.deps.protocol.user_registry, &from)
            .await;
    for detached in detached_available {
        let mut probe_response = build_available_presence(
            &detached.resource,
            &from,
            detached.show.as_ref().map(show_name),
            detached.status.as_deref(),
            detached.priority,
        );
        // Relay the detached resource's own stored extension payloads
        // (XEP-0115 caps, XEP-0319 idle, anything else) verbatim, exactly
        // like the live branch below (issue #1103).
        probe_response.payloads.extend(detached.payloads);
        let presence = Stanza::Presence(probe_response);
        for requester_resource in &requester_resources {
            let _ = state
                .deps
                .protocol
                .connection_registry
                .send_to(requester_resource, presence.clone())
                .await;
        }
    }
    for (resource, _priority) in available {
        let presence_state = state
            .deps
            .protocol
            .connection_registry
            .get_presence_state(&resource);
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
        // Relay the resource's own stored extension payloads (XEP-0115 caps,
        // XEP-0319 idle, anything else) verbatim — never server-rebuilt ones
        // (issue #1101).
        if let Some(stored) = &presence_state {
            probe_response
                .payloads
                .extend(stored.payloads.iter().cloned());
        }
        let presence = Stanza::Presence(probe_response);
        for requester_resource in &requester_resources {
            let _ = state
                .deps
                .protocol
                .connection_registry
                .send_to(requester_resource, presence.clone())
                .await;
        }
    }
}

async fn send_unsubscribed_probe_response(state: &WebSocketState, from: &BareJid, to: &BareJid) {
    let stanza = Stanza::Presence(build_subscription_presence(
        SubscriptionType::Unsubscribed,
        from,
        to,
        None,
        &[],
    ));
    for resource in
        waddle_xmpp::registry::get_resources_for_user(&state.deps.protocol.user_registry, to).await
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
