use super::*;

pub(super) async fn update_subscription_roster_state(
    state: &WebSocketState,
    storage: &DatabaseRosterStorage,
    request: &waddle_xmpp::presence::subscription::PresenceSubscriptionRequest,
    ordered_relay_origin: Option<&crate::server::routes::interpret::OrderedRelayRouteOrigin>,
) -> Result<Option<SubscriptionRosterUpdate>, crate::db::roster::RosterStorageError> {
    let existing_from_item = load_existing_roster_item(storage, &request.from, &request.to).await?;
    let mut store_from_item = true;
    let mut send_unavailable_before_unsubscribed = false;
    let mut auto_approve_subscribe = false;
    let mut forward_subscription_stanza = true;
    let mut from_item = existing_from_item
        .clone()
        .unwrap_or_else(|| RosterItem::new(request.to.clone()));
    let mut to_item = load_existing_roster_item(storage, &request.to, &request.from).await?;

    match request.subscription_type {
        SubscriptionType::Subscribe => {
            if matches!(
                from_item.subscription,
                Subscription::To | Subscription::Both
            ) {
                send_existing_subscription_ack(
                    state,
                    &request.to,
                    &request.from,
                    request.status.as_deref(),
                    &request.payloads,
                    ordered_relay_origin,
                )
                .await;
                return Ok(None);
            }
            if to_item.as_ref().is_some_and(|item| {
                item.approved
                    || matches!(item.subscription, Subscription::From | Subscription::Both)
            }) {
                SubscriptionStateMachine::apply_inbound_subscribed(&mut from_item);
                if let Some(to_item) = to_item.as_mut() {
                    SubscriptionStateMachine::apply_outbound_subscribed(to_item);
                    to_item.approved = false;
                }
                auto_approve_subscribe = true;
            } else {
                SubscriptionStateMachine::apply_outbound_subscribe(&mut from_item);
                to_item = None;
            }
        }
        SubscriptionType::Subscribed => {
            if let Some(to_item) = to_item
                .as_mut()
                .filter(|item| item.ask == Some(AskType::Subscribe))
            {
                SubscriptionStateMachine::apply_outbound_subscribed(&mut from_item);
                from_item.approved = false;
                SubscriptionStateMachine::apply_inbound_subscribed(to_item);
            } else if to_item.as_ref().is_some_and(|item| {
                item.ask.is_none()
                    && matches!(item.subscription, Subscription::To | Subscription::Both)
                    && matches!(
                        from_item.subscription,
                        Subscription::From | Subscription::Both
                    )
            }) {
                forward_subscription_stanza = true;
            } else {
                if from_item.subscription == Subscription::Remove {
                    from_item.subscription = Subscription::None;
                }
                from_item.ask = None;
                from_item.approved = true;
                to_item = None;
                forward_subscription_stanza = false;
            }
        }
        SubscriptionType::Unsubscribe => {
            if !matches!(
                from_item.subscription,
                Subscription::To | Subscription::Both
            ) && from_item.ask != Some(AskType::Subscribe)
            {
                return Ok(None);
            }
            SubscriptionStateMachine::apply_outbound_unsubscribe(&mut from_item);
            if let Some(to_item) = to_item.as_mut() {
                SubscriptionStateMachine::apply_outbound_unsubscribed(to_item);
            }
        }
        SubscriptionType::Unsubscribed => {
            let valid_recipient = to_item.as_ref().is_some_and(|item| {
                item.ask == Some(AskType::Subscribe)
                    || matches!(item.subscription, Subscription::To | Subscription::Both)
            });
            let valid_sender = matches!(
                from_item.subscription,
                Subscription::From | Subscription::Both
            );
            let cancel_preapproval = from_item.approved;
            send_unavailable_before_unsubscribed = valid_sender;
            if !valid_recipient && !valid_sender && !cancel_preapproval {
                return Ok(None);
            }
            if valid_sender {
                SubscriptionStateMachine::apply_outbound_unsubscribed(&mut from_item);
            } else if cancel_preapproval {
                from_item.approved = false;
            } else if existing_from_item.is_none() {
                store_from_item = false;
            }
            if let Some(to_item) = to_item.as_mut().filter(|_| valid_recipient) {
                SubscriptionStateMachine::apply_inbound_unsubscribed(to_item);
            }
        }
    }
    // Mutate the from-user's roster, fan out the from-push, drop the lock
    // before touching the to-user's roster. Holding both per-user mutation
    // locks simultaneously could deadlock under concurrent flows that touch
    // the same user pair in opposite roles (PR #336 review). XEP-0237 §2.6
    // ordering only applies *within* a single user's push stream, so
    // emitting from-pushes before to-pushes need not be atomic across users.
    if store_from_item {
        let (mutation, _lock) = storage
            .apply_roster_change(
                &request.from,
                RosterRowChange::Upsert(roster_item_to_row(&from_item)),
            )
            .await?;
        send_roster_push_to_resources(state, &request.from, &from_item, &mutation.version).await;
        // _lock drops at end of this block; from-user's lock released before
        // we acquire to-user's lock below.
    }
    if let Some(to_item) = to_item {
        let (mutation, _lock) = storage
            .apply_roster_change(
                &request.to,
                RosterRowChange::Upsert(roster_item_to_row(&to_item)),
            )
            .await?;
        send_roster_push_to_resources(state, &request.to, &to_item, &mutation.version).await;
    }

    Ok(Some(SubscriptionRosterUpdate {
        send_unavailable_before_unsubscribed,
        auto_approve_subscribe,
        forward_subscription_stanza,
    }))
}

pub(super) struct SubscriptionRosterUpdate {
    pub(super) send_unavailable_before_unsubscribed: bool,
    pub(super) auto_approve_subscribe: bool,
    pub(super) forward_subscription_stanza: bool,
}

#[cfg(feature = "clustering")]
pub(super) async fn send_current_roster_push_to_local_resources(
    state: &WebSocketState,
    storage: &DatabaseRosterStorage,
    user_jid: &BareJid,
    contact_jid: &BareJid,
) -> Result<(), RosterStorageError> {
    let Some(item) = load_existing_roster_item(storage, user_jid, contact_jid).await? else {
        return Ok(());
    };
    let version = storage.get_or_create_roster_version(user_jid).await?;
    send_roster_push_to_resources(state, user_jid, &item, &version).await;
    Ok(())
}

async fn send_roster_push_to_resources(
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
        if state
            .deps
            .protocol
            .connection_registry
            .try_send_to(resource, Stanza::Iq(Box::new(push)))
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
                let is_interested = state
                    .deps
                    .protocol
                    .connection_registry
                    .is_roster_interested(&resource);
                if is_interested
                    && state
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
        let _ = state
            .deps
            .protocol
            .connection_registry
            .try_send_to(&resource, Stanza::Iq(Box::new(push)));
    }
}

async fn load_existing_roster_item(
    storage: &DatabaseRosterStorage,
    user: &BareJid,
    contact: &BareJid,
) -> Result<Option<RosterItem>, RosterStorageError> {
    match storage.get_roster_item(user, contact).await {
        Ok(Some(row)) => match roster_row_to_item(row) {
            Ok(item) => Ok(Some(item)),
            Err(error) => {
                warn!(user = %user, contact = %contact, error = %error, "Failed to convert roster row");
                Ok(None)
            }
        },
        Ok(None) => Ok(None),
        Err(error) => Err(error),
    }
}

#[derive(Debug, thiserror::Error)]
enum RosterConvertError {
    #[error("invalid stored roster jid '{jid}': {error}")]
    Jid { jid: String, error: String },
    #[error("invalid stored roster subscription: {0}")]
    Subscription(String),
    #[error("invalid stored roster ask: {0}")]
    Ask(String),
}

fn roster_row_to_item(row: RosterItemRow) -> Result<RosterItem, RosterConvertError> {
    let jid = row
        .contact_jid
        .parse::<BareJid>()
        .map_err(|error| RosterConvertError::Jid {
            jid: row.contact_jid.clone(),
            error: error.to_string(),
        })?;
    let subscription = row
        .subscription
        .parse::<Subscription>()
        .map_err(|error| RosterConvertError::Subscription(error.to_string()))?;
    let ask = row
        .ask
        .as_deref()
        .map(str::parse::<AskType>)
        .transpose()
        .map_err(|error| RosterConvertError::Ask(error.to_string()))?;

    Ok(RosterItem {
        jid,
        name: row.name,
        subscription,
        ask,
        approved: row.approved,
        groups: row.groups,
    })
}

fn roster_item_to_row(item: &RosterItem) -> RosterItemRow {
    RosterItemRow {
        contact_jid: item.jid.to_string(),
        name: item.name.clone(),
        subscription: subscription_state_str(item.subscription).to_string(),
        ask: item.ask.map(ask_state_str).map(ToOwned::to_owned),
        approved: item.approved,
        groups: item.groups.clone(),
    }
}

pub(in crate::server::routes::websocket::handlers::presence) fn parse_subscription_state(
    value: &str,
) -> Subscription {
    match value {
        "to" => Subscription::To,
        "from" => Subscription::From,
        "both" => Subscription::Both,
        "remove" => Subscription::Remove,
        _ => Subscription::None,
    }
}

fn subscription_state_str(value: Subscription) -> &'static str {
    match value {
        Subscription::None => "none",
        Subscription::To => "to",
        Subscription::From => "from",
        Subscription::Both => "both",
        Subscription::Remove => "remove",
    }
}

fn ask_state_str(value: AskType) -> &'static str {
    match value {
        AskType::Subscribe => "subscribe",
    }
}
