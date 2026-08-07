use super::permissions::{
    seed_spaces_node_owners, server_permission_allowed, write_space_owner_tuple,
};
use super::*;
use crate::server::routes::websocket::ResolvedPrincipal;
use crate::space_identity::{space_jid_for_node, SpaceNode};

fn is_pubsub_attachment_or_summary_node(node: &str) -> bool {
    node.starts_with(&format!(
        "{}/",
        waddle_xmpp::xep::xep0470::PUBSUB_ATTACHMENTS_NODE_PREFIX
    )) || node.starts_with(&format!(
        "{}/",
        waddle_xmpp::xep::xep0470::NS_PUBSUB_ATTACHMENTS_SUMMARY
    ))
}

pub(super) async fn handle_pubsub_admin_request(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    target_jid: &BareJid,
    user_jid: &BareJid,
    spaces_domain: &str,
    authenticated_session: &Option<Session>,
    request: PubSubRequest,
) -> Vec<String> {
    match request {
        PubSubRequest::CreateNode { node } => {
            if is_pubsub_attachment_or_summary_node(&node) {
                return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
            }
            if target_jid.to_string() == spaces_domain {
                if server_permission_allowed(
                    state,
                    authenticated_session
                        .as_ref()
                        .map(ResolvedPrincipal::from_authenticated_session),
                    Permission::CreateSpace,
                )
                .await
                .unwrap_or(false)
                {
                    let Ok(spaces_jid) = spaces_service_bare_jid(spaces_domain) else {
                        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
                    };
                    let space_node = SpaceNode::from(node.as_str());
                    if space_jid_for_node(&spaces_jid, &space_node).is_none() {
                        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::BadRequest))];
                    }
                    match state
                        .deps
                        .protocol
                        .pubsub_storage
                        .get_or_create_node(&spaces_jid, &node)
                        .await
                    {
                        Ok((_, true)) => {
                            if let Err(error) = state
                                .deps
                                .protocol
                                .pubsub_storage
                                .update_node_config(
                                    &spaces_jid,
                                    &node,
                                    &waddle_xmpp::pubsub::NodeConfig::spaces_public(),
                                )
                                .await
                            {
                                warn!(node = %node, error = %error, "Failed to configure Spaces node");
                                return vec![iq_to_xml(build_pubsub_error(
                                    iq,
                                    PubSubError::Forbidden,
                                ))];
                            }
                            if let Err(error) = write_space_owner_tuple(
                                state,
                                &node,
                                authenticated_session.as_ref(),
                            )
                            .await
                            {
                                warn!(node = %node, error = %error, "Failed to persist Space owner tuple");
                                return vec![iq_to_xml(build_pubsub_error(
                                    iq,
                                    PubSubError::Forbidden,
                                ))];
                            }
                            seed_spaces_node_owners(state, &spaces_jid, &node, user_jid).await;
                            let response = build_pubsub_success(iq);
                            return vec![iq_to_xml(response)];
                        }
                        Ok((_, false)) => {
                            let error = build_pubsub_error(iq, PubSubError::NodeExists);
                            return vec![iq_to_xml(error)];
                        }
                        Err(error) => {
                            warn!(node = %node, error = %error, "Failed to create Spaces node");
                            let error = build_pubsub_error(iq, PubSubError::Forbidden);
                            return vec![iq_to_xml(error)];
                        }
                    }
                } else {
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }
            }

            if target_jid != user_jid {
                let error = build_pubsub_error(iq, PubSubError::Forbidden);
                return vec![iq_to_xml(error)];
            }

            let result = state
                .deps
                .protocol
                .pubsub_storage
                .get_or_create_node(target_jid, &node)
                .await;

            match result {
                Ok((_, created)) => {
                    if created {
                        debug!(node = %node, "PubSub node created via WebSocket");
                    } else {
                        debug!(node = %node, "PubSub node already exists");
                    }
                    let response = build_pubsub_success(iq);
                    vec![iq_to_xml(response)]
                }
                Err(e) => {
                    warn!("PubSub node creation failed: {}", e);
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    vec![iq_to_xml(error)]
                }
            }
        }

        PubSubRequest::ConfigureNode { node } => {
            let is_pep = is_pep_self_or_to(iq, target_jid, user_jid);
            if !crate::pubsub_authz::can_administer(
                &state.deps.protocol.pubsub_storage,
                target_jid,
                &node,
                user_jid,
                is_pep,
            )
            .await
            .unwrap_or(false)
            {
                return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
            }
            let Some(node_meta) = state
                .deps
                .protocol
                .pubsub_storage
                .get_node(target_jid, &node)
                .await
                .ok()
                .flatten()
            else {
                return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
            };
            let response = build_pubsub_configure_form_result(iq, &node, &node_meta.config);
            vec![iq_to_xml(response)]
        }

        PubSubRequest::DeleteNode { node } => {
            let is_pep = is_pep_self_or_to(iq, target_jid, user_jid);
            if !crate::pubsub_authz::can_administer(
                &state.deps.protocol.pubsub_storage,
                target_jid,
                &node,
                user_jid,
                is_pep,
            )
            .await
            .unwrap_or(false)
            {
                let error = build_pubsub_error(iq, PubSubError::Forbidden);
                return vec![iq_to_xml(error)];
            }

            let result = state
                .deps
                .protocol
                .pubsub_storage
                .delete_node(target_jid, &node)
                .await;

            match result {
                Ok(deleted) => {
                    if deleted {
                        debug!(node = %node, "PubSub node deleted via WebSocket");
                        let response = build_pubsub_success(iq);
                        vec![iq_to_xml(response)]
                    } else {
                        let error = build_pubsub_error(iq, PubSubError::NodeNotFound);
                        vec![iq_to_xml(error)]
                    }
                }
                Err(e) => {
                    warn!("PubSub node deletion failed: {}", e);
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    vec![iq_to_xml(error)]
                }
            }
        }

        PubSubRequest::Subscribe { node, jid } => {
            let subscription_jid = jid.to_bare();
            if subscription_jid != *user_jid {
                let error = build_pubsub_error(iq, PubSubError::Forbidden);
                return vec![iq_to_xml(error)];
            }

            let is_pep = is_pep_self_or_to(iq, target_jid, user_jid);
            match crate::pubsub_authz::can_subscribe(
                &state.deps.protocol.pubsub_storage,
                target_jid,
                &node,
                &subscription_jid,
                is_pep,
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => {
                    // Distinguish missing node (XEP-0060 §6.1: item-not-found) from
                    // access denial (forbidden).
                    let node_exists = state
                        .deps
                        .protocol
                        .pubsub_storage
                        .get_node(target_jid, &node)
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
                Err(e) => {
                    warn!("PubSub access check failed: {e}");
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }
            }

            match state
                .deps
                .protocol
                .pubsub_storage
                .subscribe(target_jid, &node, &jid)
                .await
            {
                Ok(sub) => {
                    let response = build_pubsub_subscribe_result(iq, &node, &jid, &sub.subid);
                    vec![iq_to_xml(response)]
                }
                Err(e) => {
                    warn!("PubSub subscribe failed: {e}");
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    vec![iq_to_xml(error)]
                }
            }
        }

        PubSubRequest::Unsubscribe { node, jid, subid } => {
            let subscription_jid = jid.to_bare();
            if subscription_jid != *user_jid {
                let error = build_pubsub_error(iq, PubSubError::Forbidden);
                return vec![iq_to_xml(error)];
            }
            let typed_subid = subid.as_deref().map(SubId::from_raw);
            match state
                .deps
                .protocol
                .pubsub_storage
                .unsubscribe(target_jid, &node, &jid, typed_subid.as_ref())
                .await
            {
                Ok(true) => {
                    let response = build_pubsub_success(iq);
                    vec![iq_to_xml(response)]
                }
                Ok(false) => {
                    let error = build_pubsub_error(iq, PubSubError::NotSubscribed);
                    vec![iq_to_xml(error)]
                }
                Err(e) => {
                    warn!("PubSub unsubscribe failed: {e}");
                    let error = build_pubsub_error(iq, PubSubError::NotSubscribed);
                    vec![iq_to_xml(error)]
                }
            }
        }
        PubSubRequest::PurgeNode { node } => {
            let is_pep = is_pep_self_or_to(iq, target_jid, user_jid);
            match crate::pubsub_authz::can_administer(
                &state.deps.protocol.pubsub_storage,
                target_jid,
                &node,
                user_jid,
                is_pep,
            )
            .await
            {
                Ok(true) => {}
                Ok(false) => {
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }
                Err(e) => {
                    warn!("PubSub purge authz failed: {e}");
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }
            }
            match state
                .deps
                .protocol
                .pubsub_storage
                .purge_node(target_jid, &node)
                .await
            {
                Ok(_) => {
                    vec![iq_to_xml(build_pubsub_success(iq))]
                }
                Err(e) => {
                    warn!("PubSub purge failed: {e}");
                    vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))]
                }
            }
        }

        PubSubRequest::ConfigureNodeSet {
            node,
            config: config_patch,
        } => {
            let is_pep = is_pep_self_or_to(iq, target_jid, user_jid);
            if !crate::pubsub_authz::can_administer(
                &state.deps.protocol.pubsub_storage,
                target_jid,
                &node,
                user_jid,
                is_pep,
            )
            .await
            .unwrap_or(false)
            {
                return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
            }
            let is_spaces_service = matches!(
                spaces_service_bare_jid(spaces_domain),
                Ok(spaces_jid) if target_jid == &spaces_jid
            );
            let existing_node = match state
                .deps
                .protocol
                .pubsub_storage
                .get_node(target_jid, &node)
                .await
            {
                Ok(Some(node)) => node,
                Ok(None) => {
                    return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
                }
                Err(error) => {
                    warn!(node = %node, error = %error, "Failed to load PubSub node before configure");
                    return vec![iq_to_xml(build_pubsub_error(
                        iq,
                        PubSubError::InternalServerError,
                    ))];
                }
            };
            let config = config_patch.apply_to(existing_node.config);
            let config = if is_spaces_service {
                let access_model = config.access_model;
                match normalize_spaces_node_config(config, config_patch.max_items.is_some()) {
                    Some(config) => config,
                    None => {
                        warn!(
                            node = %node,
                            access_model = %access_model,
                            "Rejected Spaces node configuration with unsupported access model"
                        );
                        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::BadRequest))];
                    }
                }
            } else {
                config
            };
            match state
                .deps
                .protocol
                .pubsub_storage
                .update_node_config(target_jid, &node, &config)
                .await
            {
                Ok(_) => vec![iq_to_xml(build_pubsub_success(iq))],
                Err(_) => {
                    vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))]
                }
            }
        }

        PubSubRequest::AffiliationsGet { node } => {
            let is_pep = is_pep_self_or_to(iq, target_jid, user_jid);
            if !crate::pubsub_authz::can_administer(
                &state.deps.protocol.pubsub_storage,
                target_jid,
                &node,
                user_jid,
                is_pep,
            )
            .await
            .unwrap_or(false)
            {
                return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
            }
            let rows = state
                .deps
                .protocol
                .pubsub_storage
                .list_node_affiliations(target_jid, &node)
                .await
                .unwrap_or_default();
            let response = build_pubsub_affiliations_result(iq, &node, &rows);
            vec![iq_to_xml(response)]
        }

        PubSubRequest::AffiliationsSet { node, changes } => {
            let is_pep = is_pep_self_or_to(iq, target_jid, user_jid);
            if !crate::pubsub_authz::can_administer(
                &state.deps.protocol.pubsub_storage,
                target_jid,
                &node,
                user_jid,
                is_pep,
            )
            .await
            .unwrap_or(false)
            {
                return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
            }
            for (entity, aff) in &changes {
                if let Err(e) = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .set_affiliation(target_jid, &node, entity, *aff)
                    .await
                {
                    warn!("set_affiliation failed: {e}");
                    return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
                }
            }
            vec![iq_to_xml(build_pubsub_success(iq))]
        }

        PubSubRequest::OwnerSubscriptionsGet { node } => {
            let is_pep = is_pep_self_or_to(iq, target_jid, user_jid);
            if state
                .deps
                .protocol
                .pubsub_storage
                .get_node(target_jid, &node)
                .await
                .ok()
                .flatten()
                .is_none()
            {
                return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
            }
            if !crate::pubsub_authz::can_administer(
                &state.deps.protocol.pubsub_storage,
                target_jid,
                &node,
                user_jid,
                is_pep,
            )
            .await
            .unwrap_or(false)
            {
                return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
            }
            let rows = state
                .deps
                .protocol
                .pubsub_storage
                .list_node_subscriptions(target_jid, &node)
                .await
                .unwrap_or_default();
            let response = build_pubsub_owner_subscriptions_result(iq, &node, &rows);
            vec![iq_to_xml(response)]
        }

        PubSubRequest::OwnerSubscriptionsSet { node, changes } => {
            let is_pep = is_pep_self_or_to(iq, target_jid, user_jid);
            if state
                .deps
                .protocol
                .pubsub_storage
                .get_node(target_jid, &node)
                .await
                .ok()
                .flatten()
                .is_none()
            {
                return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::NodeNotFound))];
            }
            if !crate::pubsub_authz::can_administer(
                &state.deps.protocol.pubsub_storage,
                target_jid,
                &node,
                user_jid,
                is_pep,
            )
            .await
            .unwrap_or(false)
            {
                return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
            }
            for (subscriber, state_value, subid) in changes {
                match state_value {
                    SubscriptionState::Subscribed => {
                        let already_subscribed = state
                            .deps
                            .protocol
                            .pubsub_storage
                            .list_node_subscriptions(target_jid, &node)
                            .await
                            .map(|rows| {
                                rows.iter()
                                    .any(|row| row.subscriber.to_bare() == subscriber.to_bare())
                            })
                            .unwrap_or(false);
                        if already_subscribed {
                            continue;
                        }
                        if let Err(error) = state
                            .deps
                            .protocol
                            .pubsub_storage
                            .subscribe(target_jid, &node, &subscriber)
                            .await
                        {
                            warn!("PubSub owner subscription add failed: {error}");
                            return vec![iq_to_xml(build_pubsub_error(
                                iq,
                                PubSubError::InternalServerError,
                            ))];
                        }
                    }
                    SubscriptionState::None => {
                        let typed_subid = subid.as_deref().map(SubId::from_raw);
                        if let Some(typed_subid) = typed_subid.as_ref() {
                            match state
                                .deps
                                .protocol
                                .pubsub_storage
                                .unsubscribe(target_jid, &node, &subscriber, Some(typed_subid))
                                .await
                            {
                                Ok(true) => {}
                                Ok(false) => {
                                    return vec![iq_to_xml(build_pubsub_error(
                                        iq,
                                        PubSubError::NotSubscribed,
                                    ))];
                                }
                                Err(error) => {
                                    warn!("PubSub owner subscription remove failed: {error}");
                                    return vec![iq_to_xml(build_pubsub_error(
                                        iq,
                                        PubSubError::InternalServerError,
                                    ))];
                                }
                            }
                            continue;
                        }

                        let rows = match state
                            .deps
                            .protocol
                            .pubsub_storage
                            .list_node_subscriptions(target_jid, &node)
                            .await
                        {
                            Ok(rows) => rows,
                            Err(error) => {
                                warn!("PubSub owner subscription lookup failed: {error}");
                                return vec![iq_to_xml(build_pubsub_error(
                                    iq,
                                    PubSubError::InternalServerError,
                                ))];
                            }
                        };
                        let subscriber_bare = subscriber.to_bare();
                        let matching_subids: Vec<_> = rows
                            .into_iter()
                            .filter(|row| row.subscriber.to_bare() == subscriber_bare)
                            .map(|row| row.subid)
                            .collect();
                        if matching_subids.is_empty() {
                            return vec![iq_to_xml(build_pubsub_error(
                                iq,
                                PubSubError::NotSubscribed,
                            ))];
                        }
                        for matching_subid in matching_subids {
                            if let Err(error) = state
                                .deps
                                .protocol
                                .pubsub_storage
                                .unsubscribe(target_jid, &node, &subscriber, Some(&matching_subid))
                                .await
                            {
                                warn!("PubSub owner subscription remove failed: {error}");
                                return vec![iq_to_xml(build_pubsub_error(
                                    iq,
                                    PubSubError::InternalServerError,
                                ))];
                            }
                        }
                    }
                    SubscriptionState::Pending | SubscriptionState::Unconfigured => {
                        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::BadRequest))];
                    }
                }
            }
            vec![iq_to_xml(build_pubsub_success(iq))]
        }

        PubSubRequest::Unsupported { feature } => {
            vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::UnsupportedFeature(feature),
            ))]
        }
        PubSubRequest::Publish { .. }
        | PubSubRequest::Items { .. }
        | PubSubRequest::Retract { .. } => Vec::new(),
    }
}

fn normalize_spaces_node_config(
    mut config: waddle_xmpp::pubsub::NodeConfig,
    preserve_submitted_max_items: bool,
) -> Option<waddle_xmpp::pubsub::NodeConfig> {
    // Waddle currently supports XEP-0503 public Spaces as `open` and private
    // Spaces as `whitelist`. Reject `authorize` until the XEP-0060
    // owner-approval subscription flow exists, and keep the required Spaces
    // durability/notification invariants regardless of generic PubSub form
    // fields submitted by a client.
    match config.access_model {
        waddle_xmpp::pubsub::AccessModel::Open | waddle_xmpp::pubsub::AccessModel::Whitelist => {}
        waddle_xmpp::pubsub::AccessModel::Presence
        | waddle_xmpp::pubsub::AccessModel::Roster
        | waddle_xmpp::pubsub::AccessModel::Authorize => return None,
    }
    if !preserve_submitted_max_items {
        config.max_items = u32::MAX;
    }
    config.publish_model = waddle_xmpp::pubsub::PublishModel::Publishers;
    config.persist_items = true;
    config.deliver_payloads = true;
    config.notify_retract = true;
    config.notify_delete = true;
    config.send_last_published_item = waddle_xmpp::pubsub::SendLastPublishedItem::OnSub;
    Some(config)
}
