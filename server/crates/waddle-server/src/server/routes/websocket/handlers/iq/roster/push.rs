use super::*;

// Everything in this module runs after the primary roster mutation has
// succeeded and the client's IQ result is already determined: contact-side
// updates, detached-resource lookups, and push recording are best-effort
// fanout. Failures here degrade delivery (the resource catches up at next
// bind via roster versioning) and log at warn without marking the dispatch
// span — `status=error` stays reserved for operations whose outcome actually
// failed (#1428), matching `send_blocking_pushes`.
pub(super) async fn send_roster_remove_subscription_side_effects(
    state: &WebSocketState,
    storage: &DatabaseRosterStorage,
    user_jid: &BareJid,
    removed_item: &RosterItem,
) {
    if matches!(
        removed_item.subscription,
        Subscription::To | Subscription::Both
    ) {
        send_roster_remove_subscription_stanza(
            state,
            storage,
            user_jid,
            &removed_item.jid,
            SubscriptionType::Unsubscribe,
        )
        .await;
    }
    if matches!(
        removed_item.subscription,
        Subscription::From | Subscription::Both
    ) {
        send_roster_remove_subscription_stanza(
            state,
            storage,
            user_jid,
            &removed_item.jid,
            SubscriptionType::Unsubscribed,
        )
        .await;
    }
}

async fn send_roster_remove_subscription_stanza(
    state: &WebSocketState,
    storage: &DatabaseRosterStorage,
    from: &BareJid,
    to: &BareJid,
    subscription_type: SubscriptionType,
) {
    match storage.get_roster_item(to, from).await {
        Ok(Some(row)) => match roster_row_to_item(row) {
            Ok(mut item) => {
                match subscription_type {
                    SubscriptionType::Unsubscribe => {
                        SubscriptionStateMachine::apply_outbound_unsubscribed(&mut item);
                    }
                    SubscriptionType::Unsubscribed => {
                        SubscriptionStateMachine::apply_inbound_unsubscribed(&mut item);
                    }
                    SubscriptionType::Subscribe | SubscriptionType::Subscribed => {}
                }
                match storage
                    .apply_roster_change(to, RosterRowChange::Upsert(roster_item_to_row(&item)))
                    .await
                {
                    Ok((mutation, _lock)) => {
                        // _lock held until the push enqueue below completes.
                        send_roster_push_to_all_resources(state, to, &item, &mutation.version)
                            .await;
                    }
                    Err(error) => {
                        warn!(error = %error, user = %to, contact = %from, "Failed to update contact roster after removal side effect");
                    }
                }
            }
            Err(error) => {
                warn!(error = %error, user = %to, contact = %from, "Failed to convert contact roster item after removal side effect");
            }
        },
        Ok(None) => {}
        Err(error) => {
            warn!(error = %error, user = %to, contact = %from, "Failed to load contact roster item for removal side effect");
        }
    }

    let stanza = Stanza::Presence(build_subscription_presence(
        subscription_type,
        from,
        to,
        None,
        &[],
    ));
    let live_resources = state
        .deps
        .protocol
        .connection_registry
        .get_roster_interested_resources_for_user(to);
    let mut delivered_resources = Vec::new();
    for resource in &live_resources {
        if try_deliver_registered_remote_resource(state, resource, &stanza).await {
            delivered_resources.push(resource.clone());
            continue;
        }
        if state
            .deps
            .protocol
            .connection_registry
            .try_send_to(resource, stanza.clone())
            == BroadcastOutcome::Delivered
        {
            delivered_resources.push(resource.clone());
        }
    }

    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .interested_detached_resources_for_user(to)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(error = %error, user = %to, "Failed to list detached interested resources for roster removal side effect");
            return;
        }
    };
    let detached: Vec<_> = detached
        .into_iter()
        .filter(|resource| !delivered_resources.contains(resource))
        .collect();
    for resource in detached {
        match state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(&resource, &stanza, chrono::Utc::now())
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                if try_deliver_registered_remote_resource(state, &resource, &stanza).await {
                    delivered_resources.push(resource.clone());
                    continue;
                }
                if state
                    .deps
                    .protocol
                    .connection_registry
                    .try_send_to(&resource, stanza.clone())
                    == BroadcastOutcome::Delivered
                {
                    delivered_resources.push(resource.clone());
                }
            }
            Err(error) => {
                warn!(error = %error, resource = %resource, "Failed to record roster removal subscription side effect");
            }
        }
    }
}

async fn send_roster_push_to_all_resources(
    state: &WebSocketState,
    user_jid: &BareJid,
    item: &RosterItem,
    version: &RosterVersion,
) {
    let live_resources = state
        .deps
        .protocol
        .connection_registry
        .get_roster_interested_resources_for_user(user_jid);
    let mut delivered_resources = Vec::new();
    for resource in &live_resources {
        let push = build_roster_push(
            &format!("roster-push-{}", uuid::Uuid::new_v4()),
            user_jid,
            resource,
            item,
            Some(version),
        );
        let stanza = Stanza::Iq(Box::new(push));
        if try_deliver_registered_remote_resource(state, resource, &stanza).await {
            delivered_resources.push(resource.clone());
            continue;
        }
        if state
            .deps
            .protocol
            .connection_registry
            .try_send_to(resource, stanza)
            == BroadcastOutcome::Delivered
        {
            delivered_resources.push(resource.clone());
        }
    }
    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .interested_detached_resources_for_user(user_jid)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(error = %error, user = %user_jid, "Failed to list detached resources for roster push");
            return;
        }
    };
    let detached: Vec<_> = detached
        .into_iter()
        .filter(|resource| !delivered_resources.contains(resource))
        .collect();
    for resource in detached {
        let push = build_roster_push(
            &format!("roster-push-{}", uuid::Uuid::new_v4()),
            user_jid,
            &resource,
            item,
            Some(version),
        );
        let stanza = Stanza::Iq(Box::new(push));
        match state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(&resource, &stanza, chrono::Utc::now())
            .await
        {
            Ok(true) => delivered_resources.push(resource.clone()),
            Ok(false) => {
                if try_deliver_registered_remote_resource(state, &resource, &stanza).await {
                    delivered_resources.push(resource.clone());
                    continue;
                }
                if state
                    .deps
                    .protocol
                    .connection_registry
                    .try_send_to(&resource, stanza)
                    == BroadcastOutcome::Delivered
                {
                    delivered_resources.push(resource.clone());
                }
            }
            Err(error) => {
                warn!(error = %error, resource = %resource, "Failed to record detached roster push");
            }
        }
    }
    for resource in state
        .deps
        .protocol
        .connection_registry
        .get_roster_interested_resources_for_user(user_jid)
        .into_iter()
        .filter(|resource| !delivered_resources.contains(resource))
    {
        let push = build_roster_push(
            &format!("roster-push-{}", uuid::Uuid::new_v4()),
            user_jid,
            &resource,
            item,
            Some(version),
        );
        let stanza = Stanza::Iq(Box::new(push));
        if try_deliver_registered_remote_resource(state, &resource, &stanza).await {
            continue;
        }
        let _ = state
            .deps
            .protocol
            .connection_registry
            .try_send_to(&resource, stanza);
    }
}

async fn try_deliver_registered_remote_resource(
    state: &WebSocketState,
    target: &FullJid,
    stanza: &Stanza,
) -> bool {
    #[cfg(feature = "clustering")]
    {
        let Some(bridge) = state
            .deps
            .app_state
            .clustering_claims
            .ordered_relay_delivery_bridge
            .as_ref()
        else {
            return false;
        };
        bridge
            .try_deliver_registered_remote_resource(
                target,
                stanza,
                waddle_xmpp::registry::DeliveryKind::DirectFrame,
            )
            .await
            .is_some()
    }
    #[cfg(not(feature = "clustering"))]
    {
        let _ = (state, target, stanza);
        false
    }
}

pub(crate) async fn send_roster_push_to_sibling_resources(
    state: &WebSocketState,
    user_jid: &BareJid,
    source_jid: &FullJid,
    item: &RosterItem,
    version: &RosterVersion,
) {
    let live_resources: Vec<_> = state
        .deps
        .protocol
        .connection_registry
        .get_roster_interested_resources_for_user(user_jid)
        .into_iter()
        .filter(|resource| resource != source_jid)
        .collect();
    let mut delivered_resources = Vec::new();
    for resource in &live_resources {
        let push = build_roster_push(
            &format!("roster-push-{}", uuid::Uuid::new_v4()),
            user_jid,
            resource,
            item,
            Some(version),
        );
        let stanza = Stanza::Iq(Box::new(push));
        if try_deliver_registered_remote_resource(state, resource, &stanza).await {
            delivered_resources.push(resource.clone());
            continue;
        }
        if state
            .deps
            .protocol
            .connection_registry
            .try_send_to(resource, stanza)
            == BroadcastOutcome::Delivered
        {
            delivered_resources.push(resource.clone());
        }
    }
    let detached = match state
        .deps
        .protocol
        .sm_session_registry
        .interested_detached_resources_for_user(user_jid)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(error = %error, user = %user_jid, "Failed to list detached roster-interested resources");
            return;
        }
    };
    let detached: Vec<_> = detached
        .into_iter()
        .filter(|resource| resource != source_jid)
        .filter(|resource| !delivered_resources.contains(resource))
        .collect();
    for resource in detached {
        let push = build_roster_push(
            &format!("roster-push-{}", uuid::Uuid::new_v4()),
            user_jid,
            &resource,
            item,
            Some(version),
        );
        let stanza = Stanza::Iq(Box::new(push.clone()));
        match state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(&resource, &stanza, chrono::Utc::now())
            .await
        {
            Ok(true) => delivered_resources.push(resource.clone()),
            Ok(false) => {
                let is_interested = state
                    .deps
                    .protocol
                    .connection_registry
                    .is_roster_interested(&resource);
                if is_interested {
                    if try_deliver_registered_remote_resource(state, &resource, &stanza).await {
                        delivered_resources.push(resource.clone());
                        continue;
                    }
                    if state
                        .deps
                        .protocol
                        .connection_registry
                        .try_send_to(&resource, stanza)
                        == BroadcastOutcome::Delivered
                    {
                        delivered_resources.push(resource.clone());
                    }
                }
            }
            Err(error) => {
                warn!(error = %error, resource = %resource, "Failed to record detached roster push");
            }
        }
    }
    for resource in state
        .deps
        .protocol
        .connection_registry
        .get_roster_interested_resources_for_user(user_jid)
        .into_iter()
        .filter(|resource| resource != source_jid)
        .filter(|resource| !delivered_resources.contains(resource))
    {
        let push = build_roster_push(
            &format!("roster-push-{}", uuid::Uuid::new_v4()),
            user_jid,
            &resource,
            item,
            Some(version),
        );
        let stanza = Stanza::Iq(Box::new(push));
        if try_deliver_registered_remote_resource(state, &resource, &stanza).await {
            continue;
        }
        let _ = state
            .deps
            .protocol
            .connection_registry
            .try_send_to(&resource, stanza);
    }
}
