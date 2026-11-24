use super::permissions::{
    seed_spaces_node_owners, server_permission_allowed, write_space_owner_tuple,
};
use super::{
    build_pubsub_affiliations_result, build_pubsub_configure_form_result, build_pubsub_error,
    build_pubsub_owner_subscriptions_result, build_pubsub_subscribe_result, build_pubsub_success,
    debug, is_pep_self_or_to, spaces_service_bare_jid, warn, BareJid, Permission, PubSubError,
    PubSubRequest, Session, SubId, SubscriptionState, WebSocketState,
};
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
    authenticated_session: Option<&Session>,
    request: PubSubRequest,
) -> Vec<xmpp_parsers::iq::Iq> {
    match request {
        PubSubRequest::CreateNode { node } => {
            if is_pubsub_attachment_or_summary_node(&node) {
                return vec![build_pubsub_error(iq, PubSubError::Forbidden)];
            }
            if target_jid.to_string() == spaces_domain {
                if server_permission_allowed(
                    state,
                    authenticated_session.map(ResolvedPrincipal::from_authenticated_session),
                    Permission::CreateSpace,
                )
                .await
                .unwrap_or(false)
                {
                    let Ok(spaces_jid) = spaces_service_bare_jid(spaces_domain) else {
                        return vec![build_pubsub_error(iq, PubSubError::InvalidJid)];
                    };
                    let space_node = SpaceNode::from(node.as_str());
                    if space_jid_for_node(&spaces_jid, &space_node).is_none() {
                        return vec![build_pubsub_error(iq, PubSubError::BadRequest)];
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
                                return vec![build_pubsub_error(iq, PubSubError::Forbidden)];
                            }
                            if let Err(error) =
                                write_space_owner_tuple(state, &node, authenticated_session).await
                            {
                                warn!(node = %node, error = %error, "Failed to persist Space owner tuple");
                                return vec![build_pubsub_error(iq, PubSubError::Forbidden)];
                            }
                            seed_spaces_node_owners(state, &spaces_jid, &node, user_jid).await;
                            let response = build_pubsub_success(iq);
                            return vec![response];
                        }
                        Ok((_, false)) => {
                            let error = build_pubsub_error(iq, PubSubError::NodeExists);
                            return vec![error];
                        }
                        Err(error) => {
                            warn!(node = %node, error = %error, "Failed to create Spaces node");
                            let error = build_pubsub_error(iq, PubSubError::Forbidden);
                            return vec![error];
                        }
                    }
                } else {
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    return vec![error];
                }
            }

            if target_jid != user_jid {
                let error = build_pubsub_error(iq, PubSubError::Forbidden);
                return vec![error];
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
                    vec![response]
                }
                Err(e) => {
                    warn!("PubSub node creation failed: {}", e);
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    vec![error]
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
                return vec![build_pubsub_error(iq, PubSubError::Forbidden)];
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
                return vec![build_pubsub_error(iq, PubSubError::NodeNotFound)];
            };
            let response = build_pubsub_configure_form_result(iq, &node, &node_meta.config);
            // A Space's owner is shown whether the Space is open to other
            // servers. A request without a session is a remote principal's,
            // which neither reads nor changes that setting.
            let is_spaces_service = matches!(
                spaces_service_bare_jid(spaces_domain),
                Ok(spaces_jid) if target_jid == &spaces_jid
            );
            if is_spaces_service && authenticated_session.is_some() {
                let federated = crate::server::routes::websocket::s2s_spaces::space_federated(
                    state, target_jid, &node_meta,
                )
                .await;
                let Some(federated) = federated else {
                    return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
                };
                return vec![
                    crate::server::routes::websocket::s2s_spaces::with_federated_field(
                        response, federated,
                    ),
                ];
            }
            vec![response]
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
                return vec![error];
            }

            // A Space deleted through PubSub, whoever asks, takes its rooms
            // out of it and leaves them standing: their parent grants on
            // it, their links to it and their bookmarks in it go, with the
            // Space's own relations and metadata and what S2S recorded for
            // it, so nothing is left for a Space created again under the
            // name to inherit. Only `spaces:delete` destroys rooms.
            if matches!(
                spaces_service_bare_jid(spaces_domain),
                Ok(spaces_jid) if target_jid == &spaces_jid
            ) {
                use crate::admin::spaces::SpaceDeletion;
                return match crate::admin::spaces::delete_space(
                    &state.deps.app_state,
                    spaces_service(state, target_jid),
                    &node,
                )
                .await
                {
                    SpaceDeletion::Deleted => {
                        debug!(node = %node, "Space deleted via PubSub");
                        vec![build_pubsub_success(iq)]
                    }
                    SpaceDeletion::NotFound => {
                        vec![build_pubsub_error(iq, PubSubError::NodeNotFound)]
                    }
                    SpaceDeletion::Refused => vec![build_pubsub_error(iq, PubSubError::Forbidden)],
                    SpaceDeletion::Failed => {
                        vec![build_pubsub_error(iq, PubSubError::InternalServerError)]
                    }
                };
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
                        vec![response]
                    } else {
                        let error = build_pubsub_error(iq, PubSubError::NodeNotFound);
                        vec![error]
                    }
                }
                Err(e) => {
                    warn!("PubSub node deletion failed: {}", e);
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    vec![error]
                }
            }
        }

        PubSubRequest::Subscribe { node, jid } => {
            let subscription_jid = jid.to_bare();
            if subscription_jid != *user_jid {
                let error = build_pubsub_error(iq, PubSubError::Forbidden);
                return vec![error];
            }
            // A room-bound extension node is subscribed to by the room's
            // current right, not by an affiliation an earlier read left.
            if super::extension_route_room_access(
                state,
                target_jid,
                &node,
                &subscription_jid,
                crate::permissions::Permission::View,
            )
            .await
                == super::ExtensionRouteAccess::Denied
            {
                let error = build_pubsub_error(iq, PubSubError::Forbidden);
                return vec![error];
            }

            let is_pep = is_pep_self_or_to(iq, target_jid, user_jid);
            let decision = if is_pep {
                crate::pubsub_authz::pep_read_decision(
                    &state.deps.protocol.pubsub_storage,
                    &crate::pubsub_authz::ContactDirectory::new(&state.deps.app_state.db_pool),
                    target_jid,
                    &node,
                    &subscription_jid,
                )
                .await
            } else {
                crate::pubsub_authz::can_subscribe(
                    &state.deps.protocol.pubsub_storage,
                    target_jid,
                    &node,
                    &subscription_jid,
                    false,
                )
                .await
                .map(|allowed| {
                    if allowed {
                        crate::pubsub_authz::ReadDecision::Allowed
                    } else {
                        crate::pubsub_authz::ReadDecision::Denied
                    }
                })
            };
            match decision {
                Ok(crate::pubsub_authz::ReadDecision::Allowed) => {}
                Ok(crate::pubsub_authz::ReadDecision::PresenceSubscriptionRequired) => {
                    return vec![build_pubsub_error(
                        iq,
                        PubSubError::PresenceSubscriptionRequired,
                    )];
                }
                Ok(crate::pubsub_authz::ReadDecision::NotInRosterGroup) => {
                    return vec![build_pubsub_error(iq, PubSubError::NotInRosterGroup)];
                }
                Ok(crate::pubsub_authz::ReadDecision::Denied) => {
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
                    return vec![error];
                }
                Err(e) => {
                    warn!("PubSub access check failed: {e}");
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    return vec![error];
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
                    vec![response]
                }
                Err(e) => {
                    warn!("PubSub subscribe failed: {e}");
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    vec![error]
                }
            }
        }

        PubSubRequest::Unsubscribe { node, jid, subid } => {
            let subscription_jid = jid.to_bare();
            if subscription_jid != *user_jid {
                let error = build_pubsub_error(iq, PubSubError::Forbidden);
                return vec![error];
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
                    vec![response]
                }
                Ok(false) => {
                    let error = build_pubsub_error(iq, PubSubError::NotSubscribed);
                    vec![error]
                }
                Err(e) => {
                    warn!("PubSub unsubscribe failed: {e}");
                    let error = build_pubsub_error(iq, PubSubError::NotSubscribed);
                    vec![error]
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
                    return vec![error];
                }
                Err(e) => {
                    warn!("PubSub purge authz failed: {e}");
                    let error = build_pubsub_error(iq, PubSubError::Forbidden);
                    return vec![error];
                }
            }
            // Purging a Space's items takes its rooms out of it, as
            // retracting each item would: no room keeps a parent grant on
            // the Space, or a link to it, that no item names any more. The
            // rooms stay, and so does what S2S recorded, which names the
            // node and not its items.
            if matches!(
                spaces_service_bare_jid(spaces_domain),
                Ok(spaces_jid) if target_jid == &spaces_jid
            ) {
                use crate::admin::spaces::SpaceUnlink;
                match crate::admin::spaces::unlink_space_rooms(
                    &state.deps.app_state,
                    spaces_service(state, target_jid),
                    &node,
                )
                .await
                {
                    SpaceUnlink::Unlinked => {}
                    SpaceUnlink::Refused => {
                        return vec![build_pubsub_error(iq, PubSubError::Forbidden)];
                    }
                    SpaceUnlink::Failed => {
                        return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
                    }
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
                    vec![build_pubsub_success(iq)]
                }
                Err(e) => {
                    warn!("PubSub purge failed: {e}");
                    vec![build_pubsub_error(iq, PubSubError::NodeNotFound)]
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
                return vec![build_pubsub_error(iq, PubSubError::Forbidden)];
            }
            let is_spaces_service = matches!(
                spaces_service_bare_jid(spaces_domain),
                Ok(spaces_jid) if target_jid == &spaces_jid
            );
            // A legacy Presence avatar node is repaired BEFORE the read, so
            // a configure that omits `pubsub#access_model` never carries
            // the legacy default forward and marks it as the owner's choice.
            if is_pep
                && (node == waddle_xmpp_core::pubsub::PEP_NODE_AVATAR_DATA
                    || node == waddle_xmpp_core::pubsub::PEP_NODE_AVATAR_METADATA)
            {
                if let Err(error) = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .repair_legacy_avatar_node(target_jid, &node)
                    .await
                {
                    // Fail closed: writing the marker over an unrepaired
                    // legacy node would block its repair forever.
                    warn!(node = %node, error = %error, "Failed to repair legacy avatar node before configure");
                    return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
                }
            }
            let existing_node = match state
                .deps
                .protocol
                .pubsub_storage
                .get_node(target_jid, &node)
                .await
            {
                Ok(Some(node)) => node,
                Ok(None) => {
                    return vec![build_pubsub_error(iq, PubSubError::NodeNotFound)];
                }
                Err(error) => {
                    warn!(node = %node, error = %error, "Failed to load PubSub node before configure");
                    return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
                }
            };
            // Whether the Space is open to other servers is its owner's to
            // set, in the same form (`{urn:waddle:spaces}federated`). The
            // submitted value is read before anything is written, and
            // applied once the configuration is; a request without a
            // session is a remote principal's and never changes it.
            let federated = if is_spaces_service && authenticated_session.is_some() {
                match crate::server::routes::websocket::s2s_spaces::submitted_federated(iq) {
                    Ok(federated) => federated,
                    Err(()) => {
                        return vec![build_pubsub_error(iq, PubSubError::BadRequest)];
                    }
                }
            } else {
                None
            };
            let node_created_at_ms = existing_node.created_at.timestamp_millis();
            let config = config_patch.apply_to(existing_node.config);
            // The `authorize` approval flow is not implemented, so no node
            // can be configured with it (XEP-0060 §8.1.3: not-acceptable with
            // <unsupported-access-model/>).
            if config.access_model == waddle_xmpp::pubsub::AccessModel::Authorize {
                return vec![build_pubsub_error(iq, PubSubError::UnsupportedAccessModel)];
            }
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
                        return vec![build_pubsub_error(iq, PubSubError::UnsupportedAccessModel)];
                    }
                }
            } else {
                config
            };
            // Record the owner's explicit choice BEFORE writing it, so a
            // concurrent legacy-avatar repair (a conditional write that
            // skips marked nodes) can never land after it.
            if let Err(error) = state
                .deps
                .protocol
                .pubsub_storage
                .mark_owner_configured(target_jid, &node)
                .await
            {
                warn!(node = %node, error = %error, "Failed to record owner-configured PubSub node");
                return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
            }
            match state
                .deps
                .protocol
                .pubsub_storage
                .update_node_config(target_jid, &node, &config)
                .await
            {
                Ok(_) => {
                    if let Some(federated) = federated {
                        let applied =
                            crate::server::routes::websocket::s2s_spaces::set_space_federated(
                                state,
                                target_jid,
                                &node,
                                node_created_at_ms,
                                federated,
                            )
                            .await;
                        if !applied {
                            return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
                        }
                    }
                    vec![build_pubsub_success(iq)]
                }
                Err(_) => {
                    vec![build_pubsub_error(iq, PubSubError::NodeNotFound)]
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
                return vec![build_pubsub_error(iq, PubSubError::Forbidden)];
            }
            let rows = state
                .deps
                .protocol
                .pubsub_storage
                .list_node_affiliations(target_jid, &node)
                .await
                .unwrap_or_default();
            let response = build_pubsub_affiliations_result(iq, &node, &rows);
            vec![response]
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
                return vec![build_pubsub_error(iq, PubSubError::Forbidden)];
            }
            // XEP-0060 §8.9.2.4 (Multiple Simultaneous Modifications): the
            // joined-spaces node enforces affiliation rules stricter than the
            // state chart. A change it does not support is not applied and
            // comes back in a `not-acceptable` error with the entity's
            // existing affiliation. Any change left out of that error counts
            // as a successful modification, so every permitted change of the
            // same request is applied as usual. A request naming an entity
            // twice, which the section answers with `bad-request`, is refused
            // before any of it is applied: one of its changes could otherwise
            // be applied while the other is reported as not made.
            let (changes, refused) = if is_pep
                && node == waddle_xmpp_core::pubsub::PEP_NODE_WADDLE_JOINED_SPACES
            {
                let mut named = std::collections::HashSet::with_capacity(changes.len());
                if !changes.iter().all(|(entity, _)| named.insert(entity)) {
                    return vec![build_pubsub_error(iq, PubSubError::BadRequest)];
                }
                let (permitted, unsupported): (Vec<_>, Vec<_>) =
                    changes.into_iter().partition(|(entity, affiliation)| {
                        joined_spaces_affiliation_allowed(target_jid, entity, *affiliation)
                    });
                let mut refused = Vec::with_capacity(unsupported.len());
                for (entity, _) in unsupported {
                    // The account's own ownership of its PEP node is derived
                    // (XEP-0163 §1), not stored, so the existing affiliation
                    // is the effective one.
                    match crate::pubsub_authz::effective_affiliation(
                        &state.deps.protocol.pubsub_storage,
                        target_jid,
                        &node,
                        &entity,
                        is_pep,
                    )
                    .await
                    {
                        Ok(stored) => refused.push((entity, stored)),
                        Err(error) => {
                            warn!(
                                node = %node,
                                error = %error,
                                "Failed to read a refused joined-spaces affiliation"
                            );
                            return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
                        }
                    }
                }
                (permitted, refused)
            } else {
                (changes, Vec::new())
            };
            // ADR-0020 D8 at issuance: on the Spaces service an Owner
            // administers the Space and a publisher places rooms in it, so
            // either is written only for an account of this deployment or
            // of a trusted home of the S2S snapshot the replica runs. The
            // whole request is refused before any of it is applied; a
            // standing written for another home would not be honoured at
            // the effect either.
            let is_spaces_service = matches!(
                spaces_service_bare_jid(spaces_domain),
                Ok(spaces_jid) if target_jid == &spaces_jid
            );
            if is_spaces_service
                && changes.iter().any(|(entity, affiliation)| {
                    space_affiliation_is_privileged(*affiliation)
                        && !super::muc_admin_host_grant::privileged_standing_may_be_issued(
                            state, entity,
                        )
                })
            {
                return vec![privileged_affiliation_refused(iq)];
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
                    return vec![build_pubsub_error(iq, PubSubError::Forbidden)];
                }
                // A banned account at another server keeps no subscription
                // to the Space: lifting the ban does not bring it back.
                if is_spaces_service
                    && *aff == waddle_xmpp::pubsub::Affiliation::Outcast
                    && !crate::server::routes::websocket::s2s_spaces::forget_remote_subscription(
                        state, target_jid, &node, entity,
                    )
                    .await
                {
                    return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
                }
            }
            if !refused.is_empty() {
                return vec![joined_spaces_affiliation_refused(iq, &node, &refused)];
            }
            vec![build_pubsub_success(iq)]
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
                return vec![build_pubsub_error(iq, PubSubError::NodeNotFound)];
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
                return vec![build_pubsub_error(iq, PubSubError::Forbidden)];
            }
            let rows = state
                .deps
                .protocol
                .pubsub_storage
                .list_node_subscriptions(target_jid, &node)
                .await
                .unwrap_or_default();
            let response = build_pubsub_owner_subscriptions_result(iq, &node, &rows);
            vec![response]
        }

        PubSubRequest::OwnerSubscriptionsSet { node, changes } => {
            let is_pep = is_pep_self_or_to(iq, target_jid, user_jid);
            let is_spaces_service = matches!(
                spaces_service_bare_jid(spaces_domain),
                Ok(spaces_jid) if target_jid == &spaces_jid
            );
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
                return vec![build_pubsub_error(iq, PubSubError::NodeNotFound)];
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
                return vec![build_pubsub_error(iq, PubSubError::Forbidden)];
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
                            return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
                        }
                    }
                    SubscriptionState::None => {
                        // The subscription an account at another server made
                        // to a Space ends with the owner's removal of it.
                        if is_spaces_service
                            && !crate::server::routes::websocket::s2s_spaces::forget_remote_subscription(
                                state,
                                target_jid,
                                &node,
                                &subscriber.to_bare(),
                            )
                            .await
                        {
                            return vec![build_pubsub_error(
                                iq,
                                PubSubError::InternalServerError,
                            )];
                        }
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
                                    return vec![build_pubsub_error(
                                        iq,
                                        PubSubError::NotSubscribed,
                                    )];
                                }
                                Err(error) => {
                                    warn!("PubSub owner subscription remove failed: {error}");
                                    return vec![build_pubsub_error(
                                        iq,
                                        PubSubError::InternalServerError,
                                    )];
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
                                return vec![build_pubsub_error(
                                    iq,
                                    PubSubError::InternalServerError,
                                )];
                            }
                        };
                        let subscriber_bare = subscriber.to_bare();
                        let matching_subids: Vec<_> = rows
                            .into_iter()
                            .filter(|row| row.subscriber.to_bare() == subscriber_bare)
                            .map(|row| row.subid)
                            .collect();
                        if matching_subids.is_empty() {
                            return vec![build_pubsub_error(iq, PubSubError::NotSubscribed)];
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
                                return vec![build_pubsub_error(
                                    iq,
                                    PubSubError::InternalServerError,
                                )];
                            }
                        }
                    }
                    SubscriptionState::Pending | SubscriptionState::Unconfigured => {
                        return vec![build_pubsub_error(iq, PubSubError::BadRequest)];
                    }
                }
            }
            vec![build_pubsub_success(iq)]
        }

        PubSubRequest::Unsupported { feature } => {
            vec![build_pubsub_error(
                iq,
                PubSubError::UnsupportedFeature(feature),
            )]
        }
        PubSubRequest::Publish { .. }
        | PubSubRequest::Items { .. }
        | PubSubRequest::Retract { .. } => Vec::new(),
        _ => Vec::new(),
    }
}

/// The Spaces service `spaces_jid` as the WebSocket state reaches it: its
/// PubSub store and the MUC service whose rooms its items name.
fn spaces_service<'a>(
    state: &'a WebSocketState,
    spaces_jid: &'a BareJid,
) -> crate::admin::spaces::SpaceService<'a> {
    crate::admin::spaces::SpaceService {
        storage: &state.deps.protocol.pubsub_storage,
        spaces_jid,
        muc_domain: &state.deps.service_domains.muc,
        account_domain: state.deps.auth_state.xmpp_domain.as_str(),
    }
}

/// Whether a PubSub affiliation on a Space node is a privileged grant
/// (ADR-0020 D8): the owner administers the Space, and a publisher places
/// rooms in it. Membership and a ban are not.
pub(crate) fn space_affiliation_is_privileged(
    affiliation: waddle_xmpp::pubsub::Affiliation,
) -> bool {
    use waddle_xmpp::pubsub::Affiliation;
    match affiliation {
        Affiliation::Owner | Affiliation::Publisher | Affiliation::PublishOnly => true,
        Affiliation::Member | Affiliation::Outcast | Affiliation::None => false,
    }
}

/// The answer to an affiliation change that would write a privileged
/// affiliation for an account whose home is neither served nor trusted
/// (XEP-0060 §8.9.2.4 leaves the condition to the service; `not-allowed`
/// is what room administration answers for the same refusal).
fn privileged_affiliation_refused(iq: &xmpp_parsers::iq::Iq) -> xmpp_parsers::iq::Iq {
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};
    xmpp_parsers::iq::Iq::Error {
        from: iq.to().cloned(),
        to: iq.from().cloned(),
        id: iq.id().to_owned(),
        payload: None,
        error: StanzaError::new(
            ErrorType::Cancel,
            DefinedCondition::NotAllowed,
            "en",
            "Owner and publisher affiliations are granted to accounts of this deployment and of \
             its trusted servers only.",
        ),
    }
}

/// Whether an owner may write `affiliation` for `entity` on the
/// joined-spaces node of `account`.
///
/// The node is the account's private data (XEP-0223): the account alone
/// reads and publishes it, so it supports no affiliation that would share
/// it. Another entity may be removed (`none`) or banned (`outcast`), never
/// made a member, publisher, publish-only entity or second owner (which
/// would let it configure, purge or delete the account's list). The
/// account's own JID may only be confirmed as owner or cleared; an
/// `outcast` row for it would override its derived ownership and lock it
/// out of its own node.
fn joined_spaces_affiliation_allowed(
    account: &BareJid,
    entity: &BareJid,
    affiliation: waddle_xmpp::pubsub::Affiliation,
) -> bool {
    use waddle_xmpp::pubsub::Affiliation;
    if entity == account {
        matches!(affiliation, Affiliation::Owner | Affiliation::None)
    } else {
        matches!(affiliation, Affiliation::None | Affiliation::Outcast)
    }
}

/// The answer to an owner affiliations set on the joined-spaces node that
/// holds changes the node does not support (XEP-0060 §8.9.2.4, Multiple
/// Simultaneous Modifications).
///
/// The section lets a service enforce rules stricter than its state chart
/// and says the error SHOULD be `not-acceptable` (type `modify`). It carries
/// only the `<affiliation/>` elements that caused it, each with the entity's
/// existing affiliation: `refused` holds the unsupported changes alone,
/// since the requester treats every entity left out as modified.
fn joined_spaces_affiliation_refused(
    iq: &xmpp_parsers::iq::Iq,
    node: &str,
    refused: &[(BareJid, waddle_xmpp::pubsub::Affiliation)],
) -> xmpp_parsers::iq::Iq {
    use xmpp_parsers::stanza_error::{DefinedCondition, ErrorType, StanzaError};
    let payload = match build_pubsub_affiliations_result(iq, node, refused) {
        xmpp_parsers::iq::Iq::Result { payload, .. } => payload,
        _ => None,
    };
    xmpp_parsers::iq::Iq::Error {
        from: iq.to().cloned(),
        to: iq.from().cloned(),
        id: iq.id().to_owned(),
        payload,
        error: StanzaError::new(
            ErrorType::Modify,
            DefinedCondition::NotAcceptable,
            "en",
            "The joined-spaces node is private to its account: only none and outcast \
             affiliations are written for another entity.",
        ),
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
    config.node_type = Some(waddle_xmpp_core::pubsub::PubSubNodeType::Space);
    config.publish_model = waddle_xmpp::pubsub::PublishModel::Publishers;
    config.persist_items = true;
    config.deliver_payloads = true;
    config.notify_retract = true;
    config.notify_delete = true;
    config.send_last_published_item = waddle_xmpp::pubsub::SendLastPublishedItem::OnSub;
    Some(config)
}
