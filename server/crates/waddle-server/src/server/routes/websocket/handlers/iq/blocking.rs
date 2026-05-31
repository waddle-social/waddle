use super::roster::roster_storage_for_state;
use super::*;

pub(super) async fn handle_blocking_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
    blocklist_interested: &mut bool,
    state_machine: Option<&mut waddle_xmpp::protocol::XmppStateMachine>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            iq.id(),
            response_from,
            response_to,
            not_authorized_iq_error("Authentication required."),
        )];
    };
    let user_bare = sender_jid.to_bare();
    let db = match global_database(state).await {
        Ok(db) => db,
        Err(error) => {
            warn!(error = %error, "Failed to access database for blocking IQ");
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                internal_server_error_iq_error("Internal server error."),
            )];
        }
    };
    let storage = DatabaseBlockingStorage::new(db);
    let request = match waddle_xmpp::xep::xep0191::parse_blocking_request(iq) {
        Ok(request) => request,
        Err(error) => {
            warn!(error = %error, "Invalid blocking IQ");
            return vec![build_iq_error_xml_typed(
                iq.id(),
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        }
    };

    let response = match request {
        waddle_xmpp::xep::xep0191::BlockingRequest::GetBlocklist => {
            return match storage.list_blocked_jid_entries(&user_bare).await {
                Ok(blocked) => {
                    state
                        .deps
                        .protocol
                        .connection_registry
                        .mark_blocklist_interested(sender_jid);
                    *blocklist_interested = true;
                    vec![iq_to_xml(
                        waddle_xmpp::xep::xep0191::build_blocklist_response(iq, &blocked),
                    )]
                }
                Err(error) => {
                    warn!(jid = %user_bare, error = %error, "Failed to load blocklist");
                    vec![build_iq_error_xml_typed(
                        iq.id(),
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )]
                }
            };
        }
        waddle_xmpp::xep::xep0191::BlockingRequest::Block(jids) => {
            if let Err(error) = storage.add_blocks(&user_bare, &jids).await {
                warn!(jid = %user_bare, error = %error, "Failed to add blocks");
                return vec![build_iq_error_xml_typed(
                    iq.id(),
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
            send_blocking_presence_side_effects(state, &user_bare, &jids, true, None).await;
            send_blocking_pushes(state, &user_bare, true, &jids).await;
            vec![iq_to_xml(
                waddle_xmpp::xep::xep0191::build_blocking_success(iq),
            )]
        }
        waddle_xmpp::xep::xep0191::BlockingRequest::Unblock(jids) => {
            let unblock_all = jids.is_empty();
            let unblocked_jids = if unblock_all {
                let current = match storage.list_blocked_jid_entries(&user_bare).await {
                    Ok(current) => current,
                    Err(error) => {
                        warn!(jid = %user_bare, error = %error, "Failed to load blocklist before unblock-all");
                        return vec![build_iq_error_xml_typed(
                            iq.id(),
                            response_from,
                            response_to,
                            internal_server_error_iq_error("Internal server error."),
                        )];
                    }
                };
                if let Err(error) = storage.remove_all_blocks(&user_bare).await {
                    warn!(jid = %user_bare, error = %error, "Failed to remove all blocks");
                    return vec![build_iq_error_xml_typed(
                        iq.id(),
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )];
                }
                current
            } else {
                if let Err(error) = storage.remove_blocks(&user_bare, &jids).await {
                    warn!(jid = %user_bare, error = %error, "Failed to remove blocks");
                    return vec![build_iq_error_xml_typed(
                        iq.id(),
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )];
                }
                jids
            };
            let remaining_blocklist = if unblock_all {
                Some(waddle_xmpp::protocol::Blocklist::empty())
            } else {
                match storage.list_blocked_jid_entries(&user_bare).await {
                    Ok(entries) => Some(waddle_xmpp::protocol::Blocklist::new(entries)),
                    Err(error) => {
                        warn!(jid = %user_bare, error = %error, "Failed to load blocklist after unblock for presence side effects");
                        None
                    }
                }
            };
            send_blocking_presence_side_effects(
                state,
                &user_bare,
                &unblocked_jids,
                false,
                remaining_blocklist.as_ref(),
            )
            .await;
            let push_jids = if unblock_all {
                &[][..]
            } else {
                unblocked_jids.as_slice()
            };
            send_blocking_pushes(state, &user_bare, false, push_jids).await;
            vec![iq_to_xml(
                waddle_xmpp::xep::xep0191::build_blocking_success(iq),
            )]
        }
    };

    // After a successful Block/Unblock storage mutation, mirror the
    // updated XEP-0191 list into the per-connection
    // [`waddle_xmpp::protocol::XmppStateMachine`] so the dispatcher's
    // session-state snapshot reflects the change for subsequent
    // sender / recipient passes. Without this, blocks added live on
    // a session would not take effect until the next bind (PR13's
    // load-at-bind seed). Failing to reload the storage view is
    // logged at WARN and leaves the SM blocklist unchanged: the
    // storage layer is authoritative on disk and the next bind will
    // reload, while the request itself already succeeded for the
    // client.
    if let Some(sm) = state_machine {
        match storage.list_blocked_jid_entries(&user_bare).await {
            Ok(jids) => {
                sm.set_blocklist(waddle_xmpp::protocol::Blocklist::new(jids));
            }
            Err(error) => {
                warn!(
                    jid = %user_bare,
                    %error,
                    "Failed to refresh in-memory blocklist after XEP-0191 IQ-set; \
                     dispatcher snapshot will catch up on next bind"
                );
            }
        }
    }

    response
}

async fn send_blocking_presence_side_effects(
    state: &WebSocketState,
    user_bare: &BareJid,
    jids: &[Jid],
    blocked: bool,
    remaining_blocklist: Option<&waddle_xmpp::protocol::Blocklist>,
) {
    if !blocked && remaining_blocklist.is_none() {
        warn!(
            jid = %user_bare,
            "Skipping XEP-0191 current-presence side effects because remaining blocklist is unavailable"
        );
        return;
    }

    let storage = match roster_storage_for_state(state).await {
        Ok(storage) => storage,
        Err(error) => {
            warn!(jid = %user_bare, error = %error, "Failed to access roster storage for XEP-0191 presence side effects");
            return;
        }
    };
    let subscribers = match storage.get_presence_subscribers(user_bare).await {
        Ok(subscribers) => subscribers,
        Err(error) => {
            warn!(jid = %user_bare, error = %error, "Failed to load presence subscribers for XEP-0191 presence side effects");
            return;
        }
    };

    let subscriber_bares: HashSet<BareJid> = subscribers.into_iter().collect();

    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    for jid in jids {
        if let Some(resource) = jid.resource() {
            if jid.node().is_some() {
                let target_bare = jid.to_bare();
                if subscriber_bares.contains(&target_bare) && seen.insert(jid.clone()) {
                    targets.push(jid.clone());
                }
            } else {
                for subscriber in &subscriber_bares {
                    if subscriber.domain() == jid.domain() {
                        let target = Jid::from(subscriber.with_resource(resource));
                        if seen.insert(target.clone()) {
                            targets.push(target);
                        }
                    }
                }
            }
            continue;
        }

        let single_entry_blocklist = waddle_xmpp::protocol::Blocklist::new([jid.clone()]);
        for subscriber in &subscriber_bares {
            let subscriber_jid = Jid::from(subscriber.clone());
            if single_entry_blocklist.contains_jid(&subscriber_jid)
                && seen.insert(subscriber_jid.clone())
            {
                targets.push(subscriber_jid);
            }
        }
    }

    for target in targets {
        if !blocked && remaining_blocklist.is_some_and(|blocklist| blocklist.contains_jid(&target))
        {
            continue;
        }
        if blocked {
            send_unavailable_presence_from_user_to_jid(state, user_bare, &target).await;
        } else {
            send_current_presence_from_user_to_jid(state, user_bare, &target).await;
        }
    }
}

async fn send_blocking_pushes(
    state: &WebSocketState,
    user_bare: &BareJid,
    blocked: bool,
    jids: &[Jid],
) {
    let detached_resources = match state
        .deps
        .protocol
        .sm_session_registry
        .blocklist_interested_detached_resources_for_user(user_bare)
        .await
    {
        Ok(resources) => resources,
        Err(error) => {
            warn!(jid = %user_bare, error = %error, "Failed to load detached XEP-0191 blocklist-interested resources; continuing with live fanout");
            Vec::new()
        }
    };
    let mut recorded_detached = HashSet::new();
    for resource_jid in detached_resources {
        let push = if blocked {
            match waddle_xmpp::xep::xep0191::build_block_push(&resource_jid.clone().into(), jids) {
                Ok(push) => push,
                Err(error) => {
                    warn!(jid = %user_bare, error = %error, "Skipping invalid detached XEP-0191 block push");
                    continue;
                }
            }
        } else {
            waddle_xmpp::xep::xep0191::build_unblock_push(&resource_jid.clone().into(), jids)
        };
        match state
            .deps
            .protocol
            .sm_session_registry
            .record_stanza_for_detached_blocklist_resource(
                &resource_jid,
                &Stanza::Iq(Box::new(push)),
                chrono::Utc::now(),
            )
            .await
        {
            Ok(true) => {
                recorded_detached.insert(resource_jid);
            }
            Ok(false) => {}
            Err(error) => {
                warn!(jid = %resource_jid, error = %error, "Failed to record XEP-0191 blocklist push for detached resource");
            }
        }
    }

    for resource_jid in state
        .deps
        .protocol
        .connection_registry
        .get_blocklist_interested_resources_for_user(user_bare)
    {
        if recorded_detached.contains(&resource_jid) {
            continue;
        }
        let push = if blocked {
            match waddle_xmpp::xep::xep0191::build_block_push(&resource_jid.clone().into(), jids) {
                Ok(push) => push,
                Err(error) => {
                    warn!(jid = %user_bare, error = %error, "Skipping invalid XEP-0191 block push");
                    continue;
                }
            }
        } else {
            waddle_xmpp::xep::xep0191::build_unblock_push(&resource_jid.clone().into(), jids)
        };
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource_jid, Stanza::Iq(Box::new(push)))
            .await;
    }
}
