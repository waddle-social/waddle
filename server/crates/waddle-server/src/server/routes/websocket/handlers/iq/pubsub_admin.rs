use super::permissions::{
    seed_spaces_node_owners, server_permission_allowed, write_space_owner_tuple,
};
use super::pubsub_helpers::{is_pep_self_or_to, spaces_service_bare_jid};
use super::*;

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
            if target_jid.to_string() == spaces_domain {
                if server_permission_allowed(
                    state,
                    authenticated_session.as_ref(),
                    Permission::CreateSpace,
                )
                .await
                .unwrap_or(false)
                {
                    let Ok(spaces_jid) = spaces_service_bare_jid(&spaces_domain) else {
                        return vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::InvalidJid))];
                    };
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
                                    &iq,
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
                                    &iq,
                                    PubSubError::Forbidden,
                                ))];
                            }
                            seed_spaces_node_owners(state, &spaces_jid, &node, &user_jid).await;
                            let response = build_pubsub_success(&iq);
                            return vec![iq_to_xml(response)];
                        }
                        Ok((_, false)) => {
                            let error = build_pubsub_error(&iq, PubSubError::NodeExists);
                            return vec![iq_to_xml(error)];
                        }
                        Err(error) => {
                            warn!(node = %node, error = %error, "Failed to create Spaces node");
                            let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                            return vec![iq_to_xml(error)];
                        }
                    }
                } else {
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }
            }

            if target_jid != user_jid {
                let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                return vec![iq_to_xml(error)];
            }

            let result = state
                .deps
                .protocol
                .pubsub_storage
                .get_or_create_node(&target_jid, &node)
                .await;

            match result {
                Ok((_, created)) => {
                    if created {
                        debug!(node = %node, "PubSub node created via WebSocket");
                    } else {
                        debug!(node = %node, "PubSub node already exists");
                    }
                    let response = build_pubsub_success(&iq);
                    return vec![iq_to_xml(response)];
                }
                Err(e) => {
                    warn!("PubSub node creation failed: {}", e);
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }
            }
        }

        PubSubRequest::ConfigureNode { node } => {
            let is_pep = is_pep_self_or_to(&iq, &target_jid, &user_jid);
            if !crate::pubsub_authz::can_administer(
                &state.deps.protocol.pubsub_storage,
                &target_jid,
                &node,
                &user_jid,
                is_pep,
            )
            .await
            .unwrap_or(false)
            {
                return vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::Forbidden))];
            }
            let Some(node_meta) = state
                .deps
                .protocol
                .pubsub_storage
                .get_node(&target_jid, &node)
                .await
                .ok()
                .flatten()
            else {
                return vec![iq_to_xml(build_pubsub_error(
                    &iq,
                    PubSubError::NodeNotFound,
                ))];
            };
            let response = build_pubsub_configure_form_result(&iq, &node, &node_meta.config);
            return vec![iq_to_xml(response)];
        }

        PubSubRequest::DeleteNode { node } => {
            let is_pep = is_pep_self_or_to(&iq, &target_jid, &user_jid);
            if !crate::pubsub_authz::can_administer(
                &state.deps.protocol.pubsub_storage,
                &target_jid,
                &node,
                &user_jid,
                is_pep,
            )
            .await
            .unwrap_or(false)
            {
                let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                return vec![iq_to_xml(error)];
            }

            let result = state
                .deps
                .protocol
                .pubsub_storage
                .delete_node(&target_jid, &node)
                .await;

            match result {
                Ok(deleted) => {
                    if deleted {
                        debug!(node = %node, "PubSub node deleted via WebSocket");
                        let response = build_pubsub_success(&iq);
                        return vec![iq_to_xml(response)];
                    } else {
                        let error = build_pubsub_error(&iq, PubSubError::NodeNotFound);
                        return vec![iq_to_xml(error)];
                    }
                }
                Err(e) => {
                    warn!("PubSub node deletion failed: {}", e);
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }
            }
        }

        PubSubRequest::Subscribe { node, jid } => {
            let subscription_jid = jid.to_bare();
            if subscription_jid != *user_jid {
                let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                return vec![iq_to_xml(error)];
            }

            let is_pep = is_pep_self_or_to(&iq, &target_jid, &user_jid);
            match crate::pubsub_authz::can_subscribe(
                &state.deps.protocol.pubsub_storage,
                &target_jid,
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
                        .get_node(&target_jid, &node)
                        .await
                        .ok()
                        .flatten()
                        .is_some();
                    let error = if node_exists {
                        build_pubsub_error(&iq, PubSubError::Forbidden)
                    } else {
                        build_pubsub_error(&iq, PubSubError::NodeNotFound)
                    };
                    return vec![iq_to_xml(error)];
                }
                Err(e) => {
                    warn!("PubSub access check failed: {e}");
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }
            }

            match state
                .deps
                .protocol
                .pubsub_storage
                .subscribe(&target_jid, &node, &jid)
                .await
            {
                Ok(sub) => {
                    let response = build_pubsub_subscribe_result(&iq, &node, &jid, &sub.subid);
                    return vec![iq_to_xml(response)];
                }
                Err(e) => {
                    warn!("PubSub subscribe failed: {e}");
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }
            }
        }

        PubSubRequest::Unsubscribe { node, jid, subid } => {
            let subscription_jid = jid.to_bare();
            if subscription_jid != *user_jid {
                let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                return vec![iq_to_xml(error)];
            }
            let typed_subid = subid.as_deref().map(SubId::from_raw);
            match state
                .deps
                .protocol
                .pubsub_storage
                .unsubscribe(&target_jid, &node, &jid, typed_subid.as_ref())
                .await
            {
                Ok(true) => {
                    let response = build_pubsub_success(&iq);
                    return vec![iq_to_xml(response)];
                }
                Ok(false) => {
                    let error = build_pubsub_error(&iq, PubSubError::NotSubscribed);
                    return vec![iq_to_xml(error)];
                }
                Err(e) => {
                    warn!("PubSub unsubscribe failed: {e}");
                    let error = build_pubsub_error(&iq, PubSubError::NotSubscribed);
                    return vec![iq_to_xml(error)];
                }
            }
        }
        PubSubRequest::PurgeNode { node } => {
            let is_pep = is_pep_self_or_to(&iq, &target_jid, &user_jid);
            match crate::pubsub_authz::can_administer(
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
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }
                Err(e) => {
                    warn!("PubSub purge authz failed: {e}");
                    let error = build_pubsub_error(&iq, PubSubError::Forbidden);
                    return vec![iq_to_xml(error)];
                }
            }
            match state
                .deps
                .protocol
                .pubsub_storage
                .purge_node(&target_jid, &node)
                .await
            {
                Ok(_) => return vec![iq_to_xml(build_pubsub_success(&iq))],
                Err(e) => {
                    warn!("PubSub purge failed: {e}");
                    return vec![iq_to_xml(build_pubsub_error(
                        &iq,
                        PubSubError::NodeNotFound,
                    ))];
                }
            }
        }

        PubSubRequest::ConfigureNodeSet { node, config } => {
            let is_pep = is_pep_self_or_to(&iq, &target_jid, &user_jid);
            if !crate::pubsub_authz::can_administer(
                &state.deps.protocol.pubsub_storage,
                &target_jid,
                &node,
                &user_jid,
                is_pep,
            )
            .await
            .unwrap_or(false)
            {
                return vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::Forbidden))];
            }
            match state
                .deps
                .protocol
                .pubsub_storage
                .update_node_config(&target_jid, &node, &config)
                .await
            {
                Ok(_) => return vec![iq_to_xml(build_pubsub_success(&iq))],
                Err(_) => {
                    return vec![iq_to_xml(build_pubsub_error(
                        &iq,
                        PubSubError::NodeNotFound,
                    ))];
                }
            }
        }

        PubSubRequest::AffiliationsGet { node } => {
            let is_pep = is_pep_self_or_to(&iq, &target_jid, &user_jid);
            if !crate::pubsub_authz::can_administer(
                &state.deps.protocol.pubsub_storage,
                &target_jid,
                &node,
                &user_jid,
                is_pep,
            )
            .await
            .unwrap_or(false)
            {
                return vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::Forbidden))];
            }
            let rows = state
                .deps
                .protocol
                .pubsub_storage
                .list_node_affiliations(&target_jid, &node)
                .await
                .unwrap_or_default();
            let response = build_pubsub_affiliations_result(&iq, &node, &rows);
            return vec![iq_to_xml(response)];
        }

        PubSubRequest::AffiliationsSet { node, changes } => {
            let is_pep = is_pep_self_or_to(&iq, &target_jid, &user_jid);
            if !crate::pubsub_authz::can_administer(
                &state.deps.protocol.pubsub_storage,
                &target_jid,
                &node,
                &user_jid,
                is_pep,
            )
            .await
            .unwrap_or(false)
            {
                return vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::Forbidden))];
            }
            for (entity, aff) in &changes {
                if let Err(e) = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .set_affiliation(&target_jid, &node, entity, *aff)
                    .await
                {
                    warn!("set_affiliation failed: {e}");
                    return vec![iq_to_xml(build_pubsub_error(&iq, PubSubError::Forbidden))];
                }
            }
            return vec![iq_to_xml(build_pubsub_success(&iq))];
        }

        PubSubRequest::Unsupported { feature } => {
            return vec![iq_to_xml(build_pubsub_error(
                &iq,
                PubSubError::UnsupportedFeature(feature),
            ))];
        }
        PubSubRequest::Publish { .. }
        | PubSubRequest::Items { .. }
        | PubSubRequest::Retract { .. } => Vec::new(),
    }
}
