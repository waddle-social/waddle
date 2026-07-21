use super::super::regular::{show_from_name, show_name};
use super::*;

pub(super) async fn send_subscription_presence_side_effects(
    state: &WebSocketState,
    request: &waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) {
    match request.subscription_type {
        SubscriptionType::Subscribed => {
            send_current_presence_from_user_to_user(
                state,
                &request.from,
                &request.to,
                ordered_relay_origin,
            )
            .await;
        }
        SubscriptionType::Unsubscribe => {
            send_unavailable_presence_from_user_to_user(
                state,
                &request.to,
                &request.from,
                ordered_relay_origin,
            )
            .await;
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
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) {
    let stanza = Stanza::Presence(build_subscription_presence(
        SubscriptionType::Subscribed,
        contact,
        requester,
        status,
        payloads,
    ));
    let live_resources = waddle_xmpp::registry::get_resources_for_user(
        &state.deps.protocol.user_registry,
        requester,
    )
    .await;
    if try_route_presence_to_bare_remote(state, requester, &stanza, ordered_relay_origin).await {
        return;
    }
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
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
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
        if let Some(stored) = &presence_state {
            presence.payloads.extend(stored.payloads.iter().cloned());
        }
        let stanza = Stanza::Presence(presence);
        send_stanza_to_available_user_resources_and_detached_available(
            state,
            to,
            &stanza,
            "current presence",
            ordered_relay_origin,
        )
        .await;
    }
}

pub(in crate::server::routes::websocket::handlers) async fn send_current_presence_from_user_to_jid(
    state: &WebSocketState,
    from: &BareJid,
    to: &Jid,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
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
        if let Some(stored) = &presence_state {
            presence.payloads.extend(stored.payloads.iter().cloned());
        }
        presence.to = Some(to.clone());
        send_presence_stanza_to_jid(
            state,
            to,
            Stanza::Presence(presence),
            "current presence",
            ordered_relay_origin,
        )
        .await;
    }
}

pub(in crate::server::routes::websocket::handlers) async fn send_unavailable_presence_from_user_to_user(
    state: &WebSocketState,
    from: &BareJid,
    to: &BareJid,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
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
            ordered_relay_origin,
        )
        .await;
    }
}

pub(in crate::server::routes::websocket::handlers) async fn send_unavailable_presence_from_user_to_jid(
    state: &WebSocketState,
    from: &BareJid,
    to: &Jid,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
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
            ordered_relay_origin,
        )
        .await;
    }
}

async fn send_presence_stanza_to_jid(
    state: &WebSocketState,
    to: &Jid,
    stanza: Stanza,
    context: &'static str,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) {
    if let Ok(full_to) = to.clone().try_into_full() {
        if try_route_presence_to_full_remote(state, &full_to, &stanza, ordered_relay_origin).await {
            return;
        }
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
            ordered_relay_origin,
        )
        .await;
    }
}

/// RFC 6121 §4.5.2: broadcast a server-generated
/// `<presence type='unavailable'/>` from `from` to the user's presence
/// subscribers (and to the user's own sibling resources) when a
/// presence-available session ends without the client retracting its
/// presence itself. Used by both terminal session paths: SM-detached
/// session expiry/invalidation and the unclean disconnect of a non-SM
/// session (issue #1105). Callers gate on the session having actually
/// been presence-available.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminatedPresenceBroadcastOutcome {
    Completed,
    Failed,
}

pub async fn broadcast_unavailable_for_terminated_session(
    state: &WebSocketState,
    from: &FullJid,
) -> TerminatedPresenceBroadcastOutcome {
    let Some(storage) = roster_storage(state).await else {
        return TerminatedPresenceBroadcastOutcome::Failed;
    };
    let from_bare = from.to_bare();
    let subscribers = match storage.get_presence_subscribers(&from_bare).await {
        Ok(subscribers) => subscribers,
        Err(error) => {
            warn!(error = %error, from = %from, "Failed to load presence subscribers for expired detached session");
            return TerminatedPresenceBroadcastOutcome::Failed;
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
            None,
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
        None,
    )
    .await;
    TerminatedPresenceBroadcastOutcome::Completed
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
        Ok(Some(detached)) => Some(PresenceStateSnapshot {
            show: detached.show,
            status: detached.status,
            priority: detached.priority,
            payloads: detached.payloads,
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
    /// XEP-0319 idle, anything else), relayed verbatim (issues #1101/#1103)
    /// from either the live registry or the detached (XEP-0198) session.
    payloads: Vec<Element>,
}

async fn send_stanza_to_available_user_resources_and_detached_available(
    state: &WebSocketState,
    user: &BareJid,
    stanza: &Stanza,
    context: &'static str,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) {
    if try_route_presence_to_bare_remote(state, user, stanza, ordered_relay_origin).await {
        return;
    }

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

pub(in crate::server::routes::websocket::handlers::presence) async fn try_route_presence_to_bare_remote(
    state: &WebSocketState,
    target: &BareJid,
    stanza: &Stanza,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) -> bool {
    #[cfg(feature = "clustering")]
    {
        let Some(origin) = ordered_relay_origin else {
            return false;
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
        remote_presence_outcome_consumed(
            bridge
                .try_deliver_bare_jid_remote(target, stanza, origin)
                .await,
        )
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (state, target, stanza, ordered_relay_origin);
        false
    }
}

pub(in crate::server::routes::websocket::handlers::presence::subscription) async fn try_route_presence_to_full_remote(
    state: &WebSocketState,
    target: &FullJid,
    stanza: &Stanza,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) -> bool {
    #[cfg(feature = "clustering")]
    {
        let Some(origin) = ordered_relay_origin else {
            return false;
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
        remote_presence_outcome_consumed(
            bridge
                .try_deliver_full_jid_remote(target, stanza, origin)
                .await,
        )
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (state, target, stanza, ordered_relay_origin);
        false
    }
}

#[cfg(feature = "clustering")]
fn remote_presence_outcome_consumed(
    outcome: Option<crate::server::routes::interpret::FullJidDeliveryOutcome>,
) -> bool {
    outcome.is_some_and(|outcome| outcome.suppresses_fallback())
}

#[cfg(all(test, feature = "clustering"))]
mod tests {
    use super::*;
    use crate::server::routes::interpret::FullJidDeliveryOutcome;

    #[test]
    fn remote_presence_consumes_only_definite_delivery_outcomes() {
        assert!(remote_presence_outcome_consumed(Some(
            FullJidDeliveryOutcome::Delivered
        )));
        assert!(remote_presence_outcome_consumed(Some(
            FullJidDeliveryOutcome::QueuedDetached
        )));
        #[cfg(feature = "clustering")]
        assert!(remote_presence_outcome_consumed(Some(
            FullJidDeliveryOutcome::MaybeCommitted
        )));
        assert!(!remote_presence_outcome_consumed(Some(
            FullJidDeliveryOutcome::Unavailable
        )));
        assert!(!remote_presence_outcome_consumed(Some(
            FullJidDeliveryOutcome::Dropped
        )));
        assert!(!remote_presence_outcome_consumed(None));
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
