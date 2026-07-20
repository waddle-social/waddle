use super::*;

pub(super) async fn handle_roster_iq(
    iq: &xmpp_parsers::iq::Iq,
    domain: &str,
    state: &WebSocketState,
    bound_jid: Option<&FullJid>,
    roster_interested: &mut bool,
    registry_owner: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
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
            mark_span_error("failed to access roster storage");
            warn!(error = %error, "Failed to access roster storage");
            return vec![build_xmpp_error_response(
                iq,
                XmppError::internal_server_error(None),
            )];
        }
    };

    if waddle_xmpp::roster::is_roster_get(iq) {
        return handle_roster_get(
            iq,
            &storage,
            state,
            &user_jid,
            full_jid,
            roster_interested,
            registry_owner,
        )
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
    registry_owner: Option<&std::sync::Arc<std::sync::atomic::AtomicBool>>,
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
            mark_span_error("failed to snapshot roster");
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
            mark_span_error("failed to convert roster rows");
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
    if let Some(owner) = registry_owner {
        mirror_remote_roster_interest(state, full_jid, owner).await;
    }
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

#[cfg(feature = "clustering")]
async fn mirror_remote_roster_interest(
    state: &WebSocketState,
    jid: &FullJid,
    owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
    if let Some(bridge) = state
        .deps
        .app_state
        .clustering_claims
        .ordered_relay_delivery_bridge
        .as_ref()
    {
        bridge
            .update_remote_user_resource_if_owner(
                jid,
                owner,
                crate::clustering::route_bridge::RemoteResourceStateUpdate::RosterInterested,
            )
            .await;
    }
}

#[cfg(not(feature = "clustering"))]
async fn mirror_remote_roster_interest(
    _state: &WebSocketState,
    _jid: &FullJid,
    _owner: &std::sync::Arc<std::sync::atomic::AtomicBool>,
) {
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
                    mark_span_error("failed to convert removed roster item");
                    warn!(user = %user_jid, contact = %requested.jid, error = %error, "Failed to convert removed roster item");
                    return vec![build_xmpp_error_response(
                        iq,
                        XmppError::internal_server_error(None),
                    )];
                }
            },
            Ok(None) => None,
            Err(error) => {
                mark_span_error("failed to load roster item before remove");
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
                mark_span_error("failed to remove roster item");
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
                    mark_span_error("failed to convert roster item");
                    warn!(user = %user_jid, contact = %requested.jid, error = %error, "Failed to convert roster item");
                    return vec![build_xmpp_error_response(
                        iq,
                        XmppError::internal_server_error(None),
                    )];
                }
            },
            Ok(None) => RosterItem::new(requested.jid.clone()),
            Err(error) => {
                mark_span_error("failed to load roster item");
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
                mark_span_error("failed to store roster item");
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

    if !try_remote_owner_roster_push(state, full_jid, user_jid, &push_item, &version).await {
        send_roster_push_to_sibling_resources(state, user_jid, full_jid, &push_item, &version)
            .await;
    }
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

pub(crate) mod push;
use push::{send_roster_push_to_sibling_resources, send_roster_remove_subscription_side_effects};

#[cfg(feature = "clustering")]
async fn try_remote_owner_roster_push(
    state: &WebSocketState,
    source_jid: &FullJid,
    user_jid: &BareJid,
    item: &RosterItem,
    version: &RosterVersion,
) -> bool {
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
        .try_fanout_remote_user_roster_push(source_jid, user_jid, item, version)
        .await
}

#[cfg(not(feature = "clustering"))]
async fn try_remote_owner_roster_push(
    _state: &WebSocketState,
    _source_jid: &FullJid,
    _user_jid: &BareJid,
    _item: &RosterItem,
    _version: &RosterVersion,
) -> bool {
    false
}

fn roster_target_allowed(iq: &xmpp_parsers::iq::Iq, domain: &str, user_jid: &BareJid) -> bool {
    match iq.to() {
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
