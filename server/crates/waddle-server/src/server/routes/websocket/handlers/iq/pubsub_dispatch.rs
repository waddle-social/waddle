use super::pubsub_admin::handle_pubsub_admin_request;
use super::*;
use crate::server::routes::websocket::handlers::pubsub_fanout;
use crate::server::routes::websocket::ResolvedPrincipal;

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
    let community_domain = ctx.community_domain;
    let extensions_domain = ctx.extensions_domain;
    let push_domain = ctx.push_domain;
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

        let target_jid = match iq.to() {
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
            PubSubRequest::Publish { node, item, .. } => {
                if !matches!(iq, xmpp_parsers::iq::Iq::Set { .. }) {
                    return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::BadRequest))];
                }

                if target_jid.to_string() == spaces_domain {
                    return handle_spaces_publish(
                        iq,
                        state,
                        muc_domain,
                        spaces_domain,
                        &node,
                        item,
                        authenticated_session
                            .as_ref()
                            .map(ResolvedPrincipal::from_authenticated_session),
                    )
                    .await;
                }
                if target_jid.to_string() == community_domain {
                    return handle_community_publish(
                        iq,
                        state,
                        community_domain,
                        &node,
                        item,
                        authenticated_session
                            .as_ref()
                            .map(ResolvedPrincipal::from_authenticated_session),
                    )
                    .await;
                }

                if target_jid.to_string() == push_domain {
                    // XEP-0357 publishes to a Push Service are emitted by the
                    // user's XMPP server, not by arbitrary client full JIDs.
                    // Durable server-origin publish jobs enter through the
                    // internal XEP-0060 PubSub IQ path; client WebSocket
                    // ingress is forbidden.
                    return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
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

                // RFC 363 user-managed avatar guard: when a user
                // wire-publishes to their OWN avatar / vCard4 node we
                // need the publish AND the provenance flip to be
                // atomic vs the concurrent OIDC publish chain. Both
                // sides take the same per-(BareJid) lock from
                // `profile::avatar_source::acquire_per_jid_lock`.
                // Acquired conditionally (the lock is meaningless for
                // non-PEP-self publishes) so unrelated publishes
                // aren't serialized.
                let touches_avatar_provenance = is_pep
                    && target_jid == user_jid
                    && (node == waddle_xmpp::xep::xep0084::NODE_AVATAR_DATA
                        || node == waddle_xmpp::xep::xep0084::NODE_AVATAR_METADATA
                        || node == waddle_xmpp::xep::xep0292::PEP_NODE_VCARD4);
                let _guard = if touches_avatar_provenance {
                    Some(crate::profile::acquire_per_jid_lock(state, &user_jid).await)
                } else {
                    None
                };

                if is_pep && node == waddle_xmpp::xep::xep0402::PEP_NODE {
                    if let Err(error) = validate_xep0402_bookmark_publish_request(&item) {
                        return vec![iq_to_xml(build_pubsub_error(iq, error))];
                    }
                }

                // Pre-publish reconcile for well-known PEP nodes whose
                // canonical XEP-defined config is stricter than the
                // generic `pep_default()`. Only the owner's own PEP
                // service is affected — peer fetches go through the
                // Items arm, not Publish.
                if is_pep && target_jid == user_jid {
                    reconcile_well_known_pep_node_config(state, &user_jid, &node).await;
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
                        // PEP → community feed bridge. Shadow-publish
                        // a typed feed entry on `community.<domain>`
                        // for mood / activity / tune / avatar /
                        // vCard4 PEP updates so the Feed pane
                        // surfaces user activity automatically.
                        // Throttled per-(user, kind); failure-silent
                        // (a bridge error MUST NOT fail the user's
                        // PEP publish).
                        if is_pep && target_jid == user_jid {
                            if let Ok(community_jid) =
                                state.deps.service_domains.community.parse::<BareJid>()
                            {
                                let _ = state
                                    .deps
                                    .protocol
                                    .pep_feed_bridge
                                    .observe(
                                        &state.deps.protocol.pubsub_storage,
                                        &community_jid,
                                        &user_jid,
                                        &node,
                                        &item,
                                    )
                                    .await;
                            }
                        }
                        // Provenance flip — runs while holding the
                        // per-JID lock above so an OIDC reconcile
                        // either sees the new `'user'` flag or hasn't
                        // started yet (won't race in to wipe).
                        if touches_avatar_provenance {
                            let db_actor = state.deps.app_state.db_pool.global_actor();
                            if is_user_avatar_retract(&node, &item) {
                                // User explicitly retracted their
                                // own avatar (XEP-0084 §4.3 empty
                                // `<metadata/>`) — opt back into OIDC
                                // management.
                                crate::profile::record_oidc_managed(db_actor, &user_jid).await;
                            } else {
                                crate::profile::record_self_published(db_actor, &user_jid).await;
                            }
                        }
                        let response =
                            build_pubsub_publish_result(iq, &node, &publish_result.item_id);
                        return vec![iq_to_xml(response)];
                    }
                    Err(e) => {
                        warn!("PubSub publish failed: {}", e);
                        let error =
                            build_pubsub_error(iq, pubsub_publish_error_from_xmpp_error(&e));
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
                        &user_jid,
                        &node,
                        max_items,
                        &item_ids,
                    )
                    .await;
                }

                if target_jid.to_string() == community_domain {
                    return handle_community_items(
                        iq,
                        state,
                        community_domain,
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
                        authenticated_session
                            .as_ref()
                            .map(ResolvedPrincipal::from_authenticated_session),
                        request,
                    )
                    .await;
                }

                let is_pep = is_pep_self_or_to(iq, &target_jid, &user_jid);
                match crate::pubsub_authz::can_subscribe(
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
                        let node_meta = state
                            .deps
                            .protocol
                            .pubsub_storage
                            .get_node(&target_jid, &node)
                            .await
                            .ok()
                            .flatten();
                        let is_outcast = crate::pubsub_authz::effective_affiliation(
                            &state.deps.protocol.pubsub_storage,
                            &target_jid,
                            &node,
                            &user_jid,
                            is_pep,
                        )
                        .await
                        .is_ok_and(|affiliation| affiliation.is_outcast());
                        let error = if let Some(node_meta) = node_meta {
                            if is_outcast {
                                build_pubsub_error(iq, PubSubError::Forbidden)
                            } else if !is_pep
                                && matches!(
                                    node_meta.config.access_model,
                                    waddle_xmpp::pubsub::AccessModel::Whitelist
                                )
                            {
                                build_pubsub_error(iq, PubSubError::ClosedNode)
                            } else {
                                build_pubsub_error(iq, PubSubError::Forbidden)
                            }
                        } else {
                            build_pubsub_error(iq, PubSubError::NodeNotFound)
                        };
                        return vec![iq_to_xml(error)];
                    }
                    Err(error) => {
                        warn!(
                            node = %node,
                            error = %error,
                            "Failed to authorize PubSub items access"
                        );
                        let error = build_pubsub_error(iq, PubSubError::Forbidden);
                        return vec![iq_to_xml(error)];
                    }
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
                        authenticated_session
                            .as_ref()
                            .map(ResolvedPrincipal::from_authenticated_session),
                    )
                    .await;
                }

                if target_jid.to_string() == community_domain {
                    return handle_community_retract(
                        iq,
                        state,
                        community_domain,
                        &node,
                        &item_id,
                        authenticated_session
                            .as_ref()
                            .map(ResolvedPrincipal::from_authenticated_session),
                    )
                    .await;
                }

                if target_jid != user_jid {
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }

                if node == waddle_xmpp::xep::xep0402::PEP_NODE
                    && !is_valid_xep0402_bookmark_item_id(&item_id)
                {
                    let error = build_pubsub_error(iq, PubSubError::InvalidJid);
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

/// Detect XEP-0084 §4.3's empty-`<metadata/>` "I have no avatar"
/// publish: the metadata node, payload is a `<metadata>` element with
/// no children. Used to flip `avatar_source='oidc'` so a user who
/// retracts their own picture re-opts into OIDC management.
fn is_user_avatar_retract(node: &str, item: &waddle_xmpp::pubsub::PubSubItem) -> bool {
    if node != waddle_xmpp::xep::xep0084::NODE_AVATAR_METADATA {
        return false;
    }
    let Some(payload) = item.payload.as_ref() else {
        return false;
    };
    if payload.name() != "metadata" {
        return false;
    }
    payload.children().next().is_none()
}

fn validate_xep0402_bookmark_publish_request(
    item: &waddle_xmpp::pubsub::PubSubItem,
) -> Result<(), PubSubError> {
    let Some(item_id) = item.id.as_deref() else {
        return Err(PubSubError::InvalidJid);
    };
    if !is_valid_xep0402_bookmark_item_id(item_id) {
        return Err(PubSubError::InvalidJid);
    }
    if item.payload.is_none() {
        return Err(PubSubError::BadRequest);
    }
    Ok(())
}

fn is_valid_xep0402_bookmark_item_id(item_id: &str) -> bool {
    item_id
        .parse::<BareJid>()
        .is_ok_and(|jid| jid.node().is_some())
}

fn pubsub_publish_error_from_xmpp_error(error: &waddle_xmpp::XmppError) -> PubSubError {
    match error {
        // XEP-0060 §7.1.3.3 / §7.1.3.4: typed payload-shape variants
        // carry their pubsub-error subcondition explicitly. Match on
        // type, not on substring (CLAUDE.md typed-payloads rule).
        waddle_xmpp::XmppError::PubSubPayloadRequired(_) => PubSubError::PayloadRequired,
        waddle_xmpp::XmppError::PubSubInvalidPayload(_) => PubSubError::InvalidPayload,
        waddle_xmpp::XmppError::Stanza {
            condition: waddle_xmpp::StanzaErrorCondition::BadRequest,
            ..
        } => PubSubError::BadRequest,
        waddle_xmpp::XmppError::Stanza {
            condition: waddle_xmpp::StanzaErrorCondition::ItemNotFound,
            ..
        } => PubSubError::NodeNotFound,
        waddle_xmpp::XmppError::Stanza {
            condition: waddle_xmpp::StanzaErrorCondition::Forbidden,
            ..
        }
        | waddle_xmpp::XmppError::PermissionDenied(_) => PubSubError::Forbidden,
        waddle_xmpp::XmppError::Stanza {
            condition: waddle_xmpp::StanzaErrorCondition::InternalServerError,
            ..
        }
        | waddle_xmpp::XmppError::Internal(_) => PubSubError::InternalServerError,
        _ => PubSubError::Forbidden,
    }
}

/// Bring a well-known PEP node's stored config into line with the
/// current `NodeConfig::pep_for_node` defaults BEFORE a publish lands
/// its item.
///
/// Use case: an earlier version of Waddle auto-created the
/// `urn:xmpp:vcard4` node with `AccessModel::Presence` (the bare
/// `pep_default()`). After XEP-0292 §6.1 was wired through
/// `pep_for_node` the canonical access model is `Open`, but the
/// already-created node stays on the old config until something
/// explicitly reconfigures it. A user retrying their first vCard4
/// publish after upgrading would otherwise still be invisible to
/// non-roster peers. We reconcile the config in-place so the next
/// publish lands on a spec-conformant node.
///
/// Scope is deliberately narrow: only nodes whose well-known defaults
/// are stricter than ad-hoc PEP defaults (currently `urn:xmpp:vcard4`)
/// — we don't bulk-rewrite arbitrary user node configs here.
async fn reconcile_well_known_pep_node_config(state: &WebSocketState, owner: &BareJid, node: &str) {
    if node != waddle_xmpp_core::pubsub::PEP_NODE_VCARD4
        && node != waddle_xmpp_core::pubsub::PEP_NODE_WADDLE_DND
    {
        return;
    }
    let storage = &state.deps.protocol.pubsub_storage;
    let existing = match storage.get_node(owner, node).await {
        Ok(Some(node)) => node,
        Ok(None) => return,
        Err(error) => {
            warn!(
                node,
                error = %error,
                "Failed to read PEP node config for reconcile-on-publish; \
                 letting publish proceed against whatever config is stored"
            );
            return;
        }
    };
    let canonical = waddle_xmpp_core::pubsub::NodeConfig::pep_for_node(node);
    if existing.config == canonical {
        return;
    }
    if let Err(error) = storage.update_node_config(owner, node, &canonical).await {
        warn!(
            node,
            error = %error,
            "Failed to reconcile PEP node config to XEP-defaults on publish; \
             publish will proceed against the divergent config"
        );
    }
}
