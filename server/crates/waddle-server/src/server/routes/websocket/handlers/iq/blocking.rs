use super::roster::roster_storage_for_state;
use super::*;

pub(super) async fn handle_blocking_iq(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    sender_jid: Option<&FullJid>,
    response_from: Option<&str>,
    response_to: Option<&str>,
    state_machine: Option<&mut waddle_xmpp::protocol::XmppStateMachine>,
) -> Vec<String> {
    let Some(sender_jid) = sender_jid else {
        return vec![build_iq_error_xml_typed(
            &iq.id,
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
                &iq.id,
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
                &iq.id,
                response_from,
                response_to,
                bad_request_iq_error("Malformed IQ payload."),
            )];
        }
    };

    let response = match request {
        waddle_xmpp::xep::xep0191::BlockingRequest::GetBlocklist => {
            return match storage.get_blocklist(&user_bare).await {
                Ok(blocked) => vec![iq_to_xml(
                    waddle_xmpp::xep::xep0191::build_blocklist_response(iq, &blocked),
                )],
                Err(error) => {
                    warn!(jid = %user_bare, error = %error, "Failed to load blocklist");
                    vec![build_iq_error_xml_typed(
                        &iq.id,
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
                    &iq.id,
                    response_from,
                    response_to,
                    internal_server_error_iq_error("Internal server error."),
                )];
            }
            send_blocking_presence_side_effects(state, &user_bare, &jids, true).await;
            send_blocking_pushes(state, &user_bare, true, &jids).await;
            vec![iq_to_xml(
                waddle_xmpp::xep::xep0191::build_blocking_success(iq),
            )]
        }
        waddle_xmpp::xep::xep0191::BlockingRequest::Unblock(jids) => {
            let unblocked_jids = if jids.is_empty() {
                let current = match storage.get_blocklist(&user_bare).await {
                    Ok(current) => current,
                    Err(error) => {
                        warn!(jid = %user_bare, error = %error, "Failed to load blocklist before unblock-all");
                        return vec![build_iq_error_xml_typed(
                            &iq.id,
                            response_from,
                            response_to,
                            internal_server_error_iq_error("Internal server error."),
                        )];
                    }
                };
                if let Err(error) = storage.remove_all_blocks(&user_bare).await {
                    warn!(jid = %user_bare, error = %error, "Failed to remove all blocks");
                    return vec![build_iq_error_xml_typed(
                        &iq.id,
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
                        &iq.id,
                        response_from,
                        response_to,
                        internal_server_error_iq_error("Internal server error."),
                    )];
                }
                jids
            };
            send_blocking_presence_side_effects(state, &user_bare, &unblocked_jids, false).await;
            send_blocking_pushes(state, &user_bare, false, &unblocked_jids).await;
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
        match storage.list_blocked_jids(&user_bare).await {
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
    jids: &[String],
    blocked: bool,
) {
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

    let subscriber_bares: HashSet<BareJid> = subscribers
        .into_iter()
        .filter_map(|jid| match jid.parse::<Jid>() {
            Ok(jid) => Some(jid.to_bare()),
            Err(error) => {
                warn!(jid, %error, "Skipping invalid stored roster subscriber JID");
                None
            }
        })
        .collect();

    let mut targets = Vec::new();
    let mut seen = HashSet::new();
    for jid in jids {
        let Ok(target) = jid.parse::<Jid>() else {
            warn!(
                jid,
                "Skipping invalid XEP-0191 target JID for presence side effects"
            );
            continue;
        };
        let target_bare = target.to_bare();
        if subscriber_bares.contains(&target_bare) && seen.insert(target_bare.clone()) {
            targets.push(target_bare);
        }
    }

    for target in targets {
        if blocked {
            send_unavailable_presence_from_user_to_user(state, user_bare, &target).await;
        } else {
            send_current_presence_from_user_to_user(state, user_bare, &target).await;
        }
    }
}

async fn send_blocking_pushes(
    state: &WebSocketState,
    user_bare: &BareJid,
    blocked: bool,
    jids: &[String],
) {
    for resource_jid in state
        .deps
        .protocol
        .connection_registry
        .get_resources_for_user(user_bare)
    {
        let push = if blocked {
            waddle_xmpp::xep::xep0191::build_block_push(&resource_jid.clone().into(), jids)
        } else {
            waddle_xmpp::xep::xep0191::build_unblock_push(&resource_jid.clone().into(), jids)
        };
        let _ = state
            .deps
            .protocol
            .connection_registry
            .send_to(&resource_jid, Stanza::Iq(push))
            .await;
    }
}
