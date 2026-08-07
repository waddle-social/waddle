use super::permissions::{delete_channel_parent_tuple, write_channel_parent_tuple};
use super::*;
use crate::server::routes::websocket::ResolvedPrincipal;

pub(super) async fn handle_spaces_retract(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    muc_domain: &str,
    spaces_domain: &str,
    node: &str,
    item_id: &str,
    principal: Option<ResolvedPrincipal<'_>>,
) -> Vec<String> {
    match spaces_node_mutation_allowed(state, principal, node).await {
        Ok(true) => {}
        Ok(false) => return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))],
        Err(error) => {
            warn!(node, error = %error, "Failed to authorize Spaces retract");
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::Forbidden))];
        }
    }

    let Ok(room_jid) = item_id.parse::<BareJid>() else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    if room_jid.domain().as_str() != muc_domain {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    }
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    let Ok(spaces_jid) = spaces_service_bare_jid(spaces_domain) else {
        return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::InvalidJid))];
    };
    let item_filter = [item_id.to_string()];
    let previous_item = match state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&spaces_jid, node, Some(1), &item_filter)
        .await
    {
        Ok(items) if items.is_empty() => {
            return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::ItemNotFound))];
        }
        Ok(items) => {
            let Some(item) = items.into_iter().next() else {
                return vec![iq_to_xml(build_pubsub_error(iq, PubSubError::ItemNotFound))];
            };
            item.to_pubsub_item()
        }
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to preflight Spaces item before retract");
            return vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))];
        }
    };
    let link_to_restore = match state
        .deps
        .app_state
        .channel_space_link_store
        .get(&room_jid)
        .await
    {
        Ok(Some(link)) if link.space_node == node => Some(link),
        Ok(_) => None,
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to read channel-space link before Spaces retract");
            return vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))];
        }
    };
    let parent_tuple_deleted = match delete_channel_parent_tuple(state, &channel_id, node).await {
        Ok(deleted) => deleted,
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to clear channel parent tuple before Spaces retract");
            return vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))];
        }
    };
    if link_to_restore.is_some() {
        if let Err(error) = state
            .deps
            .app_state
            .channel_space_link_store
            .clear(&room_jid)
            .await
        {
            if parent_tuple_deleted {
                let _ = write_channel_parent_tuple(state, &channel_id, node).await;
            }
            warn!(item_id, node, error = %error, "Failed to clear channel-space link before Spaces retract");
            return vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))];
        }
    }
    match state
        .deps
        .protocol
        .pubsub_storage
        .retract_item(&spaces_jid, node, item_id)
        .await
    {
        Ok(true) => match delete_channel_parent_tuple(state, &channel_id, node).await {
            Ok(_) => vec![iq_to_xml(build_pubsub_success(iq))],
            Err(error) => {
                if let Err(restore_error) = state
                    .deps
                    .protocol
                    .pubsub_storage
                    .publish_item(&spaces_jid, node, &previous_item, None, false)
                    .await
                {
                    warn!(
                        item_id,
                        node,
                        error = %restore_error,
                        "Failed to restore Spaces item after final parent tuple cleanup failure"
                    );
                }
                if parent_tuple_deleted {
                    if let Err(restore_error) =
                        write_channel_parent_tuple(state, &channel_id, node).await
                    {
                        warn!(
                            item_id,
                            node,
                            error = %restore_error,
                            "Failed to restore channel parent tuple after final cleanup failure"
                        );
                    }
                }
                if let Some(link) = link_to_restore.as_ref() {
                    if let Err(restore_error) = state
                        .deps
                        .app_state
                        .channel_space_link_store
                        .set(link)
                        .await
                    {
                        warn!(
                            item_id,
                            node,
                            error = %restore_error,
                            "Failed to restore channel-space link after final parent tuple cleanup failure"
                        );
                    }
                }
                warn!(item_id, node, error = %error, "Failed to clear channel parent tuple after Spaces retract");
                vec![iq_to_xml(build_pubsub_error(
                    iq,
                    PubSubError::InternalServerError,
                ))]
            }
        },
        Ok(false) => {
            if parent_tuple_deleted {
                if let Err(restore_error) =
                    write_channel_parent_tuple(state, &channel_id, node).await
                {
                    warn!(
                        item_id,
                        node,
                        error = %restore_error,
                        "Failed to restore channel parent tuple after Spaces retract returned item-not-found"
                    );
                }
            }
            if let Some(link) = link_to_restore.as_ref() {
                if let Err(restore_error) = state
                    .deps
                    .app_state
                    .channel_space_link_store
                    .set(link)
                    .await
                {
                    warn!(
                        item_id,
                        node,
                        error = %restore_error,
                        "Failed to restore channel-space link after Spaces retract returned item-not-found"
                    );
                }
            }
            vec![iq_to_xml(build_pubsub_error(iq, PubSubError::ItemNotFound))]
        }
        Err(error) => {
            if parent_tuple_deleted {
                if let Err(restore_error) =
                    write_channel_parent_tuple(state, &channel_id, node).await
                {
                    warn!(
                        item_id,
                        node,
                        error = %restore_error,
                        "Failed to restore channel parent tuple after Spaces retract failure"
                    );
                }
            }
            if let Some(link) = link_to_restore.as_ref() {
                if let Err(restore_error) = state
                    .deps
                    .app_state
                    .channel_space_link_store
                    .set(link)
                    .await
                {
                    warn!(
                        item_id,
                        node,
                        error = %restore_error,
                        "Failed to restore channel-space link after Spaces retract failure"
                    );
                }
            }
            warn!(item_id, node, error = %error, "Failed to retract Spaces item");
            vec![iq_to_xml(build_pubsub_error(
                iq,
                PubSubError::InternalServerError,
            ))]
        }
    }
}
