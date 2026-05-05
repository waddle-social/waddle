use super::*;

pub(super) async fn handle_roster_iq(
    iq: &xmpp_parsers::iq::Iq,
    domain: &str,
    state: &WebSocketState,
    bound_jid: Option<&FullJid>,
    roster_interested: &mut bool,
) -> Vec<String> {
    let Some(full_jid) = bound_jid else {
        return vec![build_xmpp_error_response(
            iq,
            XmppError::not_authorized(Some("Authenticated session required".to_string())),
        )];
    };
    let user_jid = full_jid.to_bare();

    if !roster_target_allowed(iq, domain, &user_jid) {
        return vec![build_xmpp_error_response(iq, XmppError::forbidden(None))];
    }

    let storage = match roster_storage_for_state(state).await {
        Ok(storage) => storage,
        Err(error) => {
            warn!(error = %error, "Failed to access roster storage");
            return vec![build_xmpp_error_response(
                iq,
                XmppError::internal_server_error(None),
            )];
        }
    };

    if waddle_xmpp::roster::is_roster_get(iq) {
        return handle_roster_get(iq, &storage, state, &user_jid, full_jid, roster_interested)
            .await;
    }
    if waddle_xmpp::roster::is_roster_set(iq) {
        return handle_roster_set(iq, &storage, state, &user_jid, full_jid).await;
    }

    vec![build_xmpp_error_response(iq, XmppError::bad_request(None))]
}

async fn handle_roster_get(
    iq: &xmpp_parsers::iq::Iq,
    storage: &DatabaseRosterStorage,
    state: &WebSocketState,
    user_jid: &BareJid,
    full_jid: &FullJid,
    roster_interested: &mut bool,
) -> Vec<String> {
    let query = match parse_roster_get(iq) {
        Ok(query) => query,
        Err(error) => return vec![build_xmpp_error_response(iq, error)],
    };

    // Atomic snapshot: items + version under the same per-user lock so the
    // returned ver always identifies the returned roster state (XEP-0237 §2.6).
    let (rows, version) = match storage.snapshot_roster(user_jid).await {
        Ok(pair) => pair,
        Err(error) => {
            warn!(user = %user_jid, error = %error, "Failed to snapshot roster");
            return vec![build_xmpp_error_response(
                iq,
                XmppError::internal_server_error(None),
            )];
        }
    };
    let items = match roster_rows_to_items(rows) {
        Ok(items) => items,
        Err(error) => {
            warn!(user = %user_jid, error = %error, "Failed to convert roster rows");
            return vec![build_xmpp_error_response(
                iq,
                XmppError::internal_server_error(None),
            )];
        }
    };

    state
        .deps
        .protocol
        .connection_registry
        .mark_roster_interested(full_jid);
    *roster_interested = true;
    // XEP-0237 §2.6: matching ver -> empty <iq type='result'/> (no <query>);
    // anything else (Absent, Bootstrap, Cached(stale)) -> full roster + ver.
    if let Some(cached) = query.ver.cached() {
        if cached == &version {
            return vec![iq_to_xml(build_roster_result_empty(iq))];
        }
    }
    vec![iq_to_xml(build_roster_result(iq, &items, Some(&version)))]
}

async fn handle_roster_set(
    iq: &xmpp_parsers::iq::Iq,
    storage: &DatabaseRosterStorage,
    state: &WebSocketState,
    user_jid: &BareJid,
    full_jid: &FullJid,
) -> Vec<String> {
    let query = match parse_roster_set(iq) {
        Ok(query) => query,
        Err(error) => return vec![build_xmpp_error_response(iq, error)],
    };
    let requested = query
        .items
        .first()
        .expect("parse_roster_set guarantees one item");

    let mut removed_item = None;
    // Hold the per-user mutation lock from the start of the storage write
    // through the end of push fanout below (XEP-0237 §2.6 — pushes for
    // mutation N must be enqueued before mutation N+1's pushes can race onto
    // the recipient socket). The guard is dropped at the end of this block.
    let (set_result, version, _user_lock) = if requested.subscription.is_remove() {
        // Snapshot the item before delete so we can run subscription side effects.
        removed_item = match storage.get_roster_item(user_jid, &requested.jid).await {
            Ok(Some(row)) => match roster_row_to_item(row) {
                Ok(item) => Some(item),
                Err(error) => {
                    warn!(user = %user_jid, contact = %requested.jid, error = %error, "Failed to convert removed roster item");
                    return vec![build_xmpp_error_response(
                        iq,
                        XmppError::internal_server_error(None),
                    )];
                }
            },
            Ok(None) => None,
            Err(error) => {
                warn!(user = %user_jid, contact = %requested.jid, error = %error, "Failed to load roster item before remove");
                return vec![build_xmpp_error_response(
                    iq,
                    XmppError::internal_server_error(None),
                )];
            }
        };
        match storage
            .apply_roster_change(user_jid, RosterRowChange::Remove(requested.jid.clone()))
            .await
        {
            Ok((mutation, lock)) => (
                RosterSetResult::Removed(requested.jid.clone()),
                mutation.version,
                lock,
            ),
            Err(RosterStorageError::ItemNotFound) => {
                return vec![build_xmpp_error_response(
                    iq,
                    XmppError::item_not_found(None),
                )];
            }
            Err(error) => {
                warn!(user = %user_jid, contact = %requested.jid, error = %error, "Failed to remove roster item");
                return vec![build_xmpp_error_response(
                    iq,
                    XmppError::internal_server_error(None),
                )];
            }
        }
    } else {
        let mut item = match storage.get_roster_item(user_jid, &requested.jid).await {
            Ok(Some(row)) => match roster_row_to_item(row) {
                Ok(item) => item,
                Err(error) => {
                    warn!(user = %user_jid, contact = %requested.jid, error = %error, "Failed to convert roster item");
                    return vec![build_xmpp_error_response(
                        iq,
                        XmppError::internal_server_error(None),
                    )];
                }
            },
            Ok(None) => RosterItem::new(requested.jid.clone()),
            Err(error) => {
                warn!(user = %user_jid, contact = %requested.jid, error = %error, "Failed to load roster item");
                return vec![build_xmpp_error_response(
                    iq,
                    XmppError::internal_server_error(None),
                )];
            }
        };

        item.name = requested.name.clone();
        item.groups = requested.groups.clone();

        match storage
            .apply_roster_change(user_jid, RosterRowChange::Upsert(roster_item_to_row(&item)))
            .await
        {
            Ok((mutation, lock)) => {
                let result = match mutation.kind {
                    RosterRowMutationKind::Added(_) => RosterSetResult::Added(item),
                    RosterRowMutationKind::Updated(_) => RosterSetResult::Updated(item),
                    RosterRowMutationKind::Removed(_) => {
                        unreachable!("Upsert never reports Removed")
                    }
                };
                (result, mutation.version, lock)
            }
            Err(error) => {
                warn!(user = %user_jid, contact = %item.jid, error = %error, "Failed to store roster item");
                return vec![build_xmpp_error_response(
                    iq,
                    XmppError::internal_server_error(None),
                )];
            }
        }
    };

    let push_item = set_result.to_push_item();
    let mut frames = Vec::new();
    if state
        .deps
        .protocol
        .connection_registry
        .is_roster_interested(full_jid)
    {
        frames.push(iq_to_xml(build_roster_push(
            &format!("roster-push-{}", uuid::Uuid::new_v4()),
            user_jid,
            full_jid,
            &push_item,
            Some(&version),
        )));
    }

    send_roster_push_to_sibling_resources(state, user_jid, full_jid, &push_item, &version).await;
    // Drop the user_jid mutation lock before invoking subscription side
    // effects on the *contact's* roster — the side-effect path acquires the
    // contact's lock, and holding two user-locks simultaneously could
    // deadlock against a concurrent flow that touches the same pair in the
    // opposite role (PR #336 review). The user_jid pushes have all been
    // enqueued by this point, so XEP-0237 §2.6 ordering for user_jid is
    // already preserved.
    drop(_user_lock);
    if let Some(item) = removed_item.as_ref() {
        send_roster_remove_subscription_side_effects(state, storage, user_jid, item).await;
    }
    frames.push(iq_to_xml(build_roster_result_empty(iq)));
    frames
}

async fn send_roster_remove_subscription_side_effects(
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
    if let Ok(Some(row)) = storage.get_roster_item(to, from).await {
        match roster_row_to_item(row) {
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
            .record_stanza_for_detached_resource(&resource, &stanza)
            .await
        {
            Ok(true) => {}
            Ok(false) => {
                let _ = state
                    .deps
                    .protocol
                    .connection_registry
                    .try_send_to(&resource, stanza.clone());
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
        if state
            .deps
            .protocol
            .connection_registry
            .try_send_to(resource, Stanza::Iq(push))
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
        let stanza = Stanza::Iq(push);
        match state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(&resource, &stanza)
            .await
        {
            Ok(true) => delivered_resources.push(resource.clone()),
            Ok(false) => {
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
        let _ = state
            .deps
            .protocol
            .connection_registry
            .try_send_to(&resource, Stanza::Iq(push));
    }
}

async fn send_roster_push_to_sibling_resources(
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
        if state
            .deps
            .protocol
            .connection_registry
            .try_send_to(resource, Stanza::Iq(push))
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
        let stanza = Stanza::Iq(push.clone());
        match state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_resource(&resource, &stanza)
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
        let _ = state
            .deps
            .protocol
            .connection_registry
            .try_send_to(&resource, Stanza::Iq(push));
    }
}

fn roster_target_allowed(iq: &xmpp_parsers::iq::Iq, domain: &str, user_jid: &BareJid) -> bool {
    match iq.to.as_ref() {
        None => true,
        Some(to) if to.to_bare() == *user_jid => true,
        Some(to) if to.resource().is_none() && to.to_bare().as_str() == domain => true,
        _ => false,
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

pub(super) async fn roster_storage_for_state(
    state: &WebSocketState,
) -> Result<DatabaseRosterStorage, RosterStorageError> {
    let db = global_database(state).await?;
    Ok(DatabaseRosterStorage::new(db))
}

fn roster_rows_to_items(rows: Vec<RosterItemRow>) -> Result<Vec<RosterItem>, RosterConvertError> {
    rows.into_iter().map(roster_row_to_item).collect()
}

fn roster_row_to_item(row: RosterItemRow) -> Result<RosterItem, RosterConvertError> {
    let jid = row
        .contact_jid
        .parse::<BareJid>()
        .map_err(|error: jid::Error| RosterConvertError::Jid {
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
        subscription: item.subscription.as_str().to_string(),
        ask: item.ask.map(|ask| ask.as_str().to_string()),
        approved: item.approved,
        groups: item.groups.clone(),
    }
}
