use super::pubsub_admin::handle_pubsub_admin_request;
use super::*;
use crate::server::routes::websocket::handlers::pubsub_fanout;

pub(super) async fn handle_pubsub_iq(
    ctx: IqHandlerContext<'_>,
    state: &WebSocketState,
    phase: &ConnectionPhase,
    authenticated_session: &Option<Session>,
) -> Vec<String> {
    let iq = ctx.iq;
    let id = ctx.id;
    let muc_domain = ctx.muc_domain;
    let spaces_domain = ctx.spaces_domain;
    let extensions_domain = ctx.extensions_domain;
    let response_from = ctx.response_from;
    let response_to = ctx.response_to;

    // PubSub / PEP (XEP-0060, XEP-0163)
    if is_pubsub_iq(iq) {
        if !phase.is_ready() {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                not_authorized_iq_error("Authentication required."),
            )];
        }

        let Some(user_jid) = phase.bound_jid().map(|jid| jid.to_bare()) else {
            return vec![build_iq_error_xml_typed(
                id,
                response_from,
                response_to,
                not_authorized_iq_error("Authentication required."),
            )];
        };

        let target_jid = match &iq.to {
            Some(to_jid) => to_jid.to_bare(),
            None => user_jid.clone(),
        };

        let request = match parse_pubsub_iq(iq) {
            Ok(req) => req,
            Err(e) => {
                warn!("Failed to parse PubSub request: {}", e);
                let error = build_pubsub_error(iq, PubSubError::InvalidJid);
                return vec![iq_to_xml(error)];
            }
        };

        debug!(?request, "Handling PubSub request via WebSocket");

        match request {
            PubSubRequest::Publish { node, item } => {
                if target_jid.to_string() == spaces_domain {
                    return handle_spaces_publish(
                        iq,
                        state,
                        muc_domain,
                        spaces_domain,
                        &node,
                        item,
                        authenticated_session.as_ref(),
                    )
                    .await;
                }

                let is_pep = is_pep_self_or_to(iq, &target_jid, &user_jid);
                match crate::pubsub_authz::can_publish(
                    &state.deps.protocol.pubsub_storage,
                    &target_jid,
                    &node,
                    &user_jid,
                    is_pep,
                )
                .await
                {
                    Ok(true) => {}
                    Ok(false) => {
                        // For PEP, before the node exists, can_publish returns false because
                        // get_node returns None. Allow PEP auto-create when the publisher is
                        // the PEP owner (target == user) — this is the standard PEP semantics.
                        if is_pep && target_jid == user_jid {
                            // PEP self-publish: fall through to auto-create path.
                        } else {
                            // For non-PEP nodes, distinguish missing node (NodeNotFound,
                            // XEP-0060 §7.1) from an existing node with access denied (Forbidden).
                            let node_exists = state
                                .deps
                                .protocol
                                .pubsub_storage
                                .get_node(&target_jid, &node)
                                .await
                                .ok()
                                .flatten()
                                .is_some();
                            let error = if node_exists {
                                build_pubsub_error(iq, PubSubError::Forbidden)
                            } else {
                                build_pubsub_error(iq, PubSubError::NodeNotFound)
                            };
                            return vec![iq_to_xml(error)];
                        }
                    }
                    Err(e) => {
                        warn!("PubSub publish authz check failed: {e}");
                        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
                    }
                }

                let result = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .publish_item(&target_jid, &node, &item, Some(&user_jid), true)
                    .await;

                match result {
                    Ok(publish_result) => {
                        debug!(
                            node = %node,
                            item_id = %publish_result.item_id,
                            created = publish_result.node_created,
                            "PubSub item published via WebSocket"
                        );
                        pubsub_fanout::fan_out_publish(
                            state,
                            pubsub_fanout::FanOutRequest {
                                owner: &target_jid,
                                node: &node,
                                published_item: &item,
                                item_id: &publish_result.item_id,
                                publisher: Some(&user_jid),
                                publisher_full: phase.bound_jid(),
                                is_pep,
                            },
                        )
                        .await;
                        let response =
                            build_pubsub_publish_result(iq, &node, &publish_result.item_id);
                        return vec![iq_to_xml(response)];
                    }
                    Err(e) => {
                        warn!("PubSub publish failed: {}", e);
                        let error = build_pubsub_error(iq, PubSubError::Forbidden);
                        return vec![iq_to_xml(error)];
                    }
                }
            }

            PubSubRequest::Items {
                node,
                max_items,
                item_ids,
            } => {
                if target_jid.to_string() == spaces_domain {
                    return handle_spaces_items(
                        iq,
                        state,
                        spaces_domain,
                        &node,
                        max_items,
                        &item_ids,
                    )
                    .await;
                }

                if target_jid.to_string() == extensions_domain {
                    let request = PubSubItemsRead {
                        target_jid: &target_jid,
                        requester_jid: &user_jid,
                        node: &node,
                        max_items,
                        item_ids: &item_ids,
                    };
                    return handle_extension_route_items(
                        iq,
                        state,
                        muc_domain,
                        authenticated_session.as_ref(),
                        request,
                    )
                    .await;
                }

                let result = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .get_items(&target_jid, &node, max_items, &item_ids)
                    .await;

                match result {
                    Ok(stored_items) => {
                        let items: Vec<_> =
                            stored_items.iter().map(|si| si.to_pubsub_item()).collect();
                        debug!(
                            node = %node,
                            count = items.len(),
                            "PubSub items retrieved via WebSocket"
                        );
                        let response = build_pubsub_items_result(iq, &node, &items);
                        return vec![iq_to_xml(response)];
                    }
                    Err(e) => {
                        warn!("PubSub items retrieval failed: {}", e);
                        let error = build_pubsub_error(iq, PubSubError::NodeNotFound);
                        return vec![iq_to_xml(error)];
                    }
                }
            }

            PubSubRequest::Retract {
                node,
                item_id,
                notify: _,
            } => {
                if target_jid.to_string() == spaces_domain {
                    return handle_spaces_retract(
                        iq,
                        state,
                        muc_domain,
                        spaces_domain,
                        &node,
                        &item_id,
                        authenticated_session.as_ref(),
                    )
                    .await;
                }

                if target_jid != user_jid {
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }

                let result = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .retract_item(&target_jid, &node, &item_id)
                    .await;

                match result {
                    Ok(retracted) => {
                        if retracted {
                            debug!(node = %node, item_id = %item_id, "PubSub item retracted via WebSocket");
                            let response = build_pubsub_success(iq);
                            return vec![iq_to_xml(response)];
                        } else {
                            let error = build_pubsub_error(iq, PubSubError::ItemNotFound);
                            return vec![iq_to_xml(error)];
                        }
                    }
                    Err(e) => {
                        warn!("PubSub retract failed: {}", e);
                        let error = build_pubsub_error(iq, PubSubError::NodeNotFound);
                        return vec![iq_to_xml(error)];
                    }
                }
            }

            request => {
                return handle_pubsub_admin_request(
                    iq,
                    state,
                    &target_jid,
                    &user_jid,
                    spaces_domain,
                    authenticated_session,
                    request,
                )
                .await;
            }
        }
    }
    Vec::new()
}
