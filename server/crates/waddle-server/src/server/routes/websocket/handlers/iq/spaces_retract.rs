use super::permissions::{delete_channel_parent_tuple, write_channel_parent_tuple};
use super::{
    build_pubsub_error, build_pubsub_success, spaces_node_mutation_allowed,
    spaces_service_bare_jid, warn, BareJid, PubSubError, WebSocketState, XmppError,
};
use crate::channel_space_links::ChannelSpaceLink;
use crate::server::routes::websocket::ResolvedPrincipal;
use waddle_xmpp::identity::CanonicalBareJid;

/// The channel whose parent grant on the Space node, and whose channel-space
/// link to it, a retract removes.
struct ParentGrant {
    room_jid: BareJid,
    channel_id: String,
}

pub(super) async fn handle_spaces_retract(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    muc_domain: &str,
    spaces_domain: &str,
    node: &str,
    item_id: &str,
    principal: Option<ResolvedPrincipal<'_>>,
) -> Vec<xmpp_parsers::iq::Iq> {
    match spaces_node_mutation_allowed(state, principal, node).await {
        Ok(true) => {}
        Ok(false) => return vec![build_pubsub_error(iq, PubSubError::Forbidden)],
        Err(error) => {
            warn!(node, error = %error, "Failed to authorize Spaces retract");
            return vec![build_pubsub_error(iq, PubSubError::Forbidden)];
        }
    }

    // The item id keys the stored item, the channel-space link and the
    // channel's parent grant; only the room's canonical text names all three.
    let exact_grant = match waddle_xmpp::xep::xep0402::parse_bookmark_item_id(item_id) {
        Ok(room_jid) => {
            if room_jid.domain().as_str() != muc_domain {
                return vec![build_pubsub_error(iq, PubSubError::InvalidJid)];
            }
            let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) else {
                return vec![build_pubsub_error(iq, PubSubError::InvalidJid)];
            };
            // A room an enrolled host-grant scope manages is placed by its
            // canonical topology only; a Spaces retract does not move it.
            match state
                .deps
                .app_state
                .host_grants
                .channel_enrolled(&channel_id)
                .await
            {
                Ok(false) => {}
                Ok(true) | Err(_) => return vec![build_pubsub_error(iq, PubSubError::Forbidden)],
            }
            Some(ParentGrant {
                room_jid,
                channel_id,
            })
        }
        Err(_) => None,
    };
    let Ok(spaces_jid) = spaces_service_bare_jid(spaces_domain) else {
        return vec![build_pubsub_error(iq, PubSubError::InvalidJid)];
    };
    let item_filter = [item_id.to_string()];
    let previous_item = match state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&spaces_jid, node, Some(1), &item_filter)
        .await
    {
        Ok(items) => match items.into_iter().next() {
            Some(item) => item.to_pubsub_item(),
            None if exact_grant.is_some() => {
                return vec![build_pubsub_error(iq, PubSubError::ItemNotFound)];
            }
            // A non-canonical id is accepted only as the exact id of a
            // stored item; otherwise it names no room.
            None => return vec![build_pubsub_error(iq, PubSubError::InvalidJid)],
        },
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to preflight Spaces item before retract");
            return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
        }
    };
    let grant = match exact_grant {
        Some(grant) => Some(grant),
        None => match folded_parent_grant(state, &spaces_jid, muc_domain, node, item_id).await {
            Ok(grant) => grant,
            Err(error) => {
                warn!(item_id, node, error = %error, "Failed to resolve the room of a non-canonical Spaces item");
                return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
            }
        },
    };

    let link_to_restore = match grant.as_ref() {
        Some(grant) => match state
            .deps
            .app_state
            .channel_space_link_store
            .get(&grant.room_jid)
            .await
        {
            Ok(Some(link)) if link.space_node == node => Some(link),
            Ok(_) => None,
            Err(error) => {
                warn!(item_id, node, error = %error, "Failed to read channel-space link before Spaces retract");
                return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
            }
        },
        None => None,
    };
    let parent_tuple_deleted = match grant.as_ref() {
        Some(grant) => match delete_channel_parent_tuple(state, &grant.channel_id, node).await {
            Ok(deleted) => deleted,
            Err(error) => {
                warn!(item_id, node, error = %error, "Failed to clear channel parent tuple before Spaces retract");
                return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
            }
        },
        None => false,
    };
    if let (Some(grant), Some(_)) = (grant.as_ref(), link_to_restore.as_ref()) {
        if let Err(error) = state
            .deps
            .app_state
            .channel_space_link_store
            .clear(&grant.room_jid)
            .await
        {
            if parent_tuple_deleted {
                let _ = write_channel_parent_tuple(state, &grant.channel_id, node).await;
            }
            warn!(item_id, node, error = %error, "Failed to clear channel-space link before Spaces retract");
            return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
        }
    }
    let restore = SpacesRetractRestore {
        state,
        node,
        item_id,
        grant: grant.as_ref(),
        parent_tuple_deleted,
        link: link_to_restore.as_ref(),
    };
    match state
        .deps
        .protocol
        .pubsub_storage
        .retract_item(&spaces_jid, node, item_id)
        .await
    {
        Ok(true) => {
            let Some(grant) = grant.as_ref() else {
                return vec![build_pubsub_success(iq)];
            };
            match delete_channel_parent_tuple(state, &grant.channel_id, node).await {
                Ok(_) => vec![build_pubsub_success(iq)],
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
                    restore.run("final parent tuple cleanup failure").await;
                    warn!(item_id, node, error = %error, "Failed to clear channel parent tuple after Spaces retract");
                    vec![build_pubsub_error(iq, PubSubError::InternalServerError)]
                }
            }
        }
        Ok(false) => {
            restore.run("retract returned item-not-found").await;
            vec![build_pubsub_error(iq, PubSubError::ItemNotFound)]
        }
        Err(error) => {
            restore.run("retract failure").await;
            warn!(item_id, node, error = %error, "Failed to retract Spaces item");
            vec![build_pubsub_error(iq, PubSubError::InternalServerError)]
        }
    }
}

/// The parent grant a stored item with a non-canonical id holds.
///
/// Earlier releases stored Spaces item ids as written and gave the channel
/// the `jid` parser folds the id to (`Room@muc.example.com` folds to
/// `room@muc.example.com`) a parent tuple on the node. Retracting such an
/// item removes that grant only when no item under the room's canonical id
/// remains on the node, because that item holds the same tuple: a retract
/// revokes a grant no stored item names any more, never one another item
/// still names. `None` when the id folds to no managed room of `muc_domain`
/// (no grant was written for it) or when the canonical item remains.
async fn folded_parent_grant(
    state: &WebSocketState,
    spaces_jid: &BareJid,
    muc_domain: &str,
    node: &str,
    item_id: &str,
) -> Result<Option<ParentGrant>, XmppError> {
    let Some(room_jid) = item_id
        .parse::<BareJid>()
        .ok()
        .and_then(|folded| CanonicalBareJid::canonicalize(&folded).ok())
        .map(CanonicalBareJid::into_bare_jid)
    else {
        return Ok(None);
    };
    if room_jid.domain().as_str() != muc_domain {
        return Ok(None);
    }
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&room_jid) else {
        return Ok(None);
    };
    let canonical_item = [room_jid.to_string()];
    let remaining = state
        .deps
        .protocol
        .pubsub_storage
        .get_items(spaces_jid, node, Some(1), &canonical_item)
        .await?;
    if !remaining.is_empty() {
        return Ok(None);
    }
    Ok(Some(ParentGrant {
        room_jid,
        channel_id,
    }))
}

/// What a Spaces retract removed before the item itself, put back when the
/// retract does not complete.
struct SpacesRetractRestore<'a> {
    state: &'a WebSocketState,
    node: &'a str,
    item_id: &'a str,
    grant: Option<&'a ParentGrant>,
    parent_tuple_deleted: bool,
    link: Option<&'a ChannelSpaceLink>,
}

impl SpacesRetractRestore<'_> {
    async fn run(&self, after: &'static str) {
        let (state, node, item_id) = (self.state, self.node, self.item_id);
        if let (Some(grant), true) = (self.grant, self.parent_tuple_deleted) {
            if let Err(restore_error) =
                write_channel_parent_tuple(state, &grant.channel_id, node).await
            {
                warn!(
                    item_id,
                    node,
                    after,
                    error = %restore_error,
                    "Failed to restore channel parent tuple after an incomplete Spaces retract"
                );
            }
        }
        if let Some(link) = self.link {
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
                    after,
                    error = %restore_error,
                    "Failed to restore channel-space link after an incomplete Spaces retract"
                );
            }
        }
    }
}
