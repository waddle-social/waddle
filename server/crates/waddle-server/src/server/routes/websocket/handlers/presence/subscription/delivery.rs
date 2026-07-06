use super::super::regular::{show_from_name, show_name};
use super::*;

pub(super) async fn send_subscription_presence_side_effects(
    state: &WebSocketState,
    request: &waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
) {
    match request.subscription_type {
        SubscriptionType::Subscribed => {
            send_current_presence_from_user_to_user(state, &request.from, &request.to).await;
        }
        SubscriptionType::Unsubscribe => {
            send_unavailable_presence_from_user_to_user(state, &request.to, &request.from).await;
        }
        SubscriptionType::Subscribe | SubscriptionType::Unsubscribed => {}
    }
}

pub(super) async fn send_existing_subscription_ack(
    state: &WebSocketState,
    contact: &BareJid,
    requester: &BareJid,
    status: Option<&str>,
    payloads: &[Element],
) {
    let stanza = Stanza::Presence(build_subscription_presence(
        SubscriptionType::Subscribed,
        contact,
        requester,
        status,
        payloads,
    ));
    let live_resources = state
        .deps
        .protocol
        .connection_registry
        .get_resources_for_user(requester);
    for resource in &live_resources {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(resource, stanza.clone())
            .await;
    }
    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .interested_detached_resources_for_user(requester)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(error = %error, user = %requester, "Failed to list detached interested resources");
            return;
        }
    };
    for resource in detached
        .into_iter()
        .filter(|resource| !live_resources.contains(resource))
    {
        match state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(&resource, &stanza, chrono::Utc::now())
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                let _ = state
                    .deps
                    .protocol
                    .connection_registry
                    .send_to(&resource, stanza.clone())
                    .await;
            }
            Err(error) => {
                warn!(error = %error, resource = %resource, "Failed to record detached subscription acknowledgement");
            }
        }
    }
}

pub(in crate::server::routes::websocket::handlers) async fn send_current_presence_from_user_to_user(
    state: &WebSocketState,
    from: &BareJid,
    to: &BareJid,
) {
    for resource in available_live_and_detached_resources_for_user(state, from).await {
        let presence_state = presence_state_for_available_resource(state, &resource).await;
        let mut presence = build_available_presence(
            &resource,
            to,
            presence_state
                .as_ref()
                .and_then(|state| state.show.as_ref())
                .map(show_name),
            presence_state
                .as_ref()
                .and_then(|state| state.status.as_deref()),
            presence_state
                .as_ref()
                .map(|state| state.priority)
                .unwrap_or(0),
        );
        // Relay the resource's own stored extension payloads (XEP-0115 caps,
        // XEP-0319 idle, anything else) verbatim so a subscription-approval
        // push carries the contact's real advertisements (issue #1101).
        if let Some(state) = &presence_state {
            presence.payloads.extend(state.payloads.iter().cloned());
        }
        let stanza = Stanza::Presence(presence);
        send_stanza_to_available_user_resources_and_detached_available(
            state,
            to,
            &stanza,
            "current presence",
        )
        .await;
    }
}

pub(in crate::server::routes::websocket::handlers) async fn send_current_presence_from_user_to_jid(
    state: &WebSocketState,
    from: &BareJid,
    to: &Jid,
) {
    for resource in available_live_and_detached_resources_for_user(state, from).await {
        let presence_state = presence_state_for_available_resource(state, &resource).await;
        let mut presence = build_available_presence(
            &resource,
            &to.to_bare(),
            presence_state
                .as_ref()
                .and_then(|state| state.show.as_ref())
                .map(show_name),
            presence_state
                .as_ref()
                .and_then(|state| state.status.as_deref()),
            presence_state
                .as_ref()
                .map(|state| state.priority)
                .unwrap_or(0),
        );
        // Relay the resource's own stored extension payloads (XEP-0115 caps,
        // XEP-0319 idle, anything else) verbatim (issue #1101).
        if let Some(state) = &presence_state {
            presence.payloads.extend(state.payloads.iter().cloned());
        }
        presence.to = Some(to.clone());
        send_presence_stanza_to_jid(state, to, Stanza::Presence(presence), "current presence")
            .await;
    }
}

pub(in crate::server::routes::websocket::handlers) async fn send_unavailable_presence_from_user_to_user(
    state: &WebSocketState,
    from: &BareJid,
    to: &BareJid,
) {
    for resource in available_live_and_detached_resources_for_user(state, from).await {
        let mut presence =
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
        presence.from = Some(Jid::from(resource));
        presence.to = Some(Jid::from(to.clone()));
        let stanza = Stanza::Presence(presence);
        send_stanza_to_available_user_resources_and_detached_available(
            state,
            to,
            &stanza,
            "unavailable presence",
        )
        .await;
    }
}

pub(in crate::server::routes::websocket::handlers) async fn send_unavailable_presence_from_user_to_jid(
    state: &WebSocketState,
    from: &BareJid,
    to: &Jid,
) {
    for resource in available_live_and_detached_resources_for_user(state, from).await {
        let mut presence =
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
        presence.from = Some(Jid::from(resource));
        presence.to = Some(to.clone());
        send_presence_stanza_to_jid(
            state,
            to,
            Stanza::Presence(presence),
            "unavailable presence",
        )
        .await;
    }
}

async fn send_presence_stanza_to_jid(
    state: &WebSocketState,
    to: &Jid,
    stanza: Stanza,
    context: &'static str,
) {
    if let Ok(full_to) = to.clone().try_into_full() {
        let sent = state
            .deps
            .protocol
            .connection_registry
            .send_to(&full_to, stanza.clone())
            .await
            .is_sent();
        if !sent {
            match state
                .deps
                .protocol
                .sm_session_registry
                .record_stanza_for_detached_bound_resource(&full_to, &stanza, chrono::Utc::now())
                .await
            {
                Ok(_) => {}
                Err(error) => {
                    warn!(
                        resource = %full_to,
                        error = %error,
                        context,
                        "Failed to stash exact-JID presence side effect for detached resource"
                    );
                }
            }
        }
    } else {
        send_stanza_to_available_user_resources_and_detached_available(
            state,
            &to.to_bare(),
            &stanza,
            context,
        )
        .await;
    }
}

pub async fn broadcast_unavailable_for_expired_detached_session(
    state: &WebSocketState,
    from: &FullJid,
) {
    let Some(storage) = roster_storage(state).await else {
        return;
    };
    let from_bare = from.to_bare();
    let subscribers = match storage.get_presence_subscribers(&from_bare).await {
        Ok(subscribers) => subscribers,
        Err(error) => {
            warn!(error = %error, from = %from, "Failed to load presence subscribers for expired detached session");
            return;
        }
    };

    for subscriber in subscribers {
        let mut presence =
            xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
        presence.from = Some(Jid::from(from.clone()));
        presence.to = Some(Jid::from(subscriber.clone()));
        let stanza = Stanza::Presence(presence);
        send_stanza_to_available_user_resources_and_detached_available(
            state,
            &subscriber,
            &stanza,
            "expired detached unavailable presence",
        )
        .await;
    }

    let mut presence =
        xmpp_parsers::presence::Presence::new(xmpp_parsers::presence::Type::Unavailable);
    presence.from = Some(Jid::from(from.clone()));
    presence.to = Some(Jid::from(from_bare.clone()));
    let stanza = Stanza::Presence(presence);
    send_stanza_to_available_user_resources_and_detached_available(
        state,
        &from_bare,
        &stanza,
        "expired detached unavailable presence to siblings",
    )
    .await;
}

async fn available_live_and_detached_resources_for_user(
    state: &WebSocketState,
    user: &BareJid,
) -> Vec<FullJid> {
    let mut resources: Vec<FullJid> = state
        .deps
        .protocol
        .connection_registry
        .get_available_resources_for_user(user)
        .into_iter()
        .map(|(jid, _)| jid)
        .collect();
    match state
        .deps
        .protocol
        .sm_session_registry
        .available_detached_resources_for_user(user)
        .await
    {
        Ok(detached) => {
            for resource in detached {
                if !resources.contains(&resource) {
                    resources.push(resource);
                }
            }
        }
        Err(error) => {
            warn!(error = %error, user = %user, "Failed to list detached available resources");
        }
    }
    resources
}

async fn presence_state_for_available_resource(
    state: &WebSocketState,
    resource: &FullJid,
) -> Option<PresenceStateSnapshot> {
    if let Some(state) = state
        .deps
        .protocol
        .connection_registry
        .get_presence_state(resource)
    {
        return Some(PresenceStateSnapshot {
            show: state.show.as_deref().and_then(show_from_name),
            status: state.status,
            priority: state.priority,
            payloads: state.payloads,
        });
    }
    match state
        .deps
        .protocol
        .sm_session_registry
        .detached_presence_state(resource)
        .await
    {
        Ok(Some((show, status, priority))) => Some(PresenceStateSnapshot {
            show,
            status,
            priority,
            // Detached (XEP-0198) presence state does not yet persist
            // extension payloads (idle, caps, ...).
            payloads: Vec::new(),
        }),
        Ok(None) => None,
        Err(error) => {
            warn!(error = %error, resource = %resource, "Failed to read detached presence state");
            None
        }
    }
}

struct PresenceStateSnapshot {
    show: Option<xmpp_parsers::presence::Show>,
    status: Option<String>,
    priority: i8,
    /// The resource's original presence extension payloads (XEP-0115 caps,
    /// XEP-0319 idle, anything else), relayed verbatim (issue #1101). Only the
    /// live registry persists them; the detached (XEP-0198) path has none yet.
    payloads: Vec<Element>,
}

async fn send_stanza_to_available_user_resources_and_detached_available(
    state: &WebSocketState,
    user: &BareJid,
    stanza: &Stanza,
    context: &'static str,
) {
    let live_resources: Vec<FullJid> = state
        .deps
        .protocol
        .connection_registry
        .get_available_resources_for_user(user)
        .into_iter()
        .map(|(jid, _)| jid)
        .collect();
    let mut delivered_resources = Vec::new();
    for resource in &live_resources {
        if state
            .deps
            .protocol
            .connection_registry
            .send_to(resource, stanza.clone())
            .await
            .is_sent()
        {
            delivered_resources.push(resource.clone());
        }
    }
    let recorded_resources = record_stanza_for_detached_available_resources_excluding(
        state,
        user,
        stanza,
        context,
        &delivered_resources,
    )
    .await;
    delivered_resources.extend(recorded_resources);
    for resource in state
        .deps
        .protocol
        .connection_registry
        .get_available_resources_for_user(user)
        .into_iter()
        .map(|(jid, _)| jid)
        .filter(|resource| !delivered_resources.contains(resource))
    {
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource, stanza.clone())
            .await;
    }
}

pub(super) async fn record_stanza_for_detached_available_resources(
    state: &WebSocketState,
    user: &BareJid,
    stanza: &Stanza,
    context: &'static str,
) -> Vec<FullJid> {
    record_stanza_for_detached_available_resources_excluding(state, user, stanza, context, &[])
        .await
}

pub(in crate::server::routes::websocket::handlers::presence) async fn record_stanza_for_detached_available_resources_excluding(
    state: &WebSocketState,
    user: &BareJid,
    stanza: &Stanza,
    context: &'static str,
    excluded_resources: &[FullJid],
) -> Vec<FullJid> {
    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .available_detached_resources_for_user(user)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(error = %error, user = %user, context, "Failed to list detached available resources");
            return Vec::new();
        }
    };

    let mut recorded = Vec::new();
    for resource in detached
        .into_iter()
        .filter(|resource| !excluded_resources.contains(resource))
    {
        match state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_available_resource(&resource, stanza, chrono::Utc::now())
            .await
        {
            Ok(true) => recorded.push(resource.clone()),
            Ok(false) => {
                let _ = state
                    .deps
                    .protocol
                    .connection_registry
                    .send_to(&resource, stanza.clone())
                    .await;
            }
            Err(error) => {
                warn!(error = %error, resource = %resource, context, "Failed to record detached available stanza");
            }
        }
    }
    recorded
}

pub(super) async fn record_subscription_stanza_for_detached_resources_excluding(
    state: &WebSocketState,
    request: &waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
    stanza: &Stanza,
    excluded_resources: &[FullJid],
) -> usize {
    if request.subscription_type == SubscriptionType::Subscribe {
        return record_stanza_for_detached_available_resources_excluding(
            state,
            &request.to,
            stanza,
            "subscription presence",
            excluded_resources,
        )
        .await
        .len();
    }

    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .interested_detached_resources_for_user(&request.to)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(error = %error, user = %request.to, "Failed to list detached interested resources");
            return 0;
        }
    };

    let mut recorded = 0;
    for resource in detached
        .into_iter()
        .filter(|resource| !excluded_resources.contains(resource))
    {
        match state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(&resource, stanza, chrono::Utc::now())
            .await
        {
            Ok(true) => recorded += 1,
            Ok(false) => {
                let _ = state
                    .deps
                    .protocol
                    .connection_registry
                    .send_to(&resource, stanza.clone())
                    .await;
            }
            Err(error) => {
                warn!(error = %error, resource = %resource, "Failed to record detached subscription stanza");
            }
        }
    }
    recorded
}

pub(super) fn subscription_presence_recipients(
    state: &WebSocketState,
    request: &waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
) -> Vec<FullJid> {
    match request.subscription_type {
        SubscriptionType::Subscribe => state
            .deps
            .protocol
            .connection_registry
            .get_available_resources_for_user(&request.to)
            .into_iter()
            .map(|(jid, _)| jid)
            .collect(),
        SubscriptionType::Subscribed
        | SubscriptionType::Unsubscribe
        | SubscriptionType::Unsubscribed => state
            .deps
            .protocol
            .connection_registry
            .get_roster_interested_resources_for_user(&request.to),
    }
}
