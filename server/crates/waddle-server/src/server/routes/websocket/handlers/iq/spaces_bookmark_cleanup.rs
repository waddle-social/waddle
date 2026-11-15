use super::permissions::{
    channel_parent_spaces, delete_channel_parent_tuple, write_channel_parent_tuple,
};
use super::{warn, BareJid, PubSubItem, WebSocketState};

/// Move a placed channel out of every Space other than `keep_node`: retract
/// its bookmark `item_id` from each of `stale_nodes`, and remove each other
/// Space as the channel's parent, including a Space that is its parent
/// without listing a bookmark of it. On a failure everything removed so far
/// is restored and the error is returned.
pub(super) async fn cleanup_stale_space_bookmarks(
    state: &WebSocketState,
    spaces_jid: &BareJid,
    channel_id: &str,
    keep_node: &str,
    item_id: &str,
    stale_nodes: &[String],
) -> Result<(), String> {
    let mut removed_stale: Vec<RemovedStaleSpaceBookmark> = Vec::new();
    for stale in stale_nodes.iter().filter(|name| name.as_str() != keep_node) {
        let item_filter = [item_id.to_string()];
        let previous_item = match state
            .deps
            .protocol
            .pubsub_storage
            .get_items(spaces_jid, stale, Some(1), &item_filter)
            .await
        {
            Ok(items) => items
                .into_iter()
                .next()
                .map(|stored| stored.to_pubsub_item()),
            Err(error) => {
                restore_stale_space_bookmarks(state, spaces_jid, channel_id, &removed_stale).await;
                return Err(format!(
                    "pubsub read stale channel bookmark from {stale} failed: {error}"
                ));
            }
        };
        let parent_tuple_deleted = match delete_channel_parent_tuple(state, channel_id, stale).await
        {
            Ok(deleted) => deleted,
            Err(error) => {
                restore_stale_space_bookmarks(state, spaces_jid, channel_id, &removed_stale).await;
                return Err(error);
            }
        };
        match state
            .deps
            .protocol
            .pubsub_storage
            .retract_item(spaces_jid, stale, item_id)
            .await
        {
            Ok(_) => {
                removed_stale.push(RemovedStaleSpaceBookmark {
                    node: stale.clone(),
                    item: previous_item,
                    parent_tuple_deleted,
                });
                match delete_channel_parent_tuple(state, channel_id, stale).await {
                    Ok(deleted) => {
                        if deleted {
                            if let Some(removed) = removed_stale.last_mut() {
                                removed.parent_tuple_deleted = true;
                            }
                        }
                    }
                    Err(error) => {
                        restore_stale_space_bookmarks(
                            state,
                            spaces_jid,
                            channel_id,
                            &removed_stale,
                        )
                        .await;
                        return Err(error);
                    }
                }
            }
            Err(error) => {
                if parent_tuple_deleted {
                    let _ = write_channel_parent_tuple(state, channel_id, stale).await;
                }
                restore_stale_space_bookmarks(state, spaces_jid, channel_id, &removed_stale).await;
                return Err(format!(
                    "pubsub retract stale channel bookmark from {stale} failed: {error}"
                ));
            }
        }
    }
    let parents = match channel_parent_spaces(state, channel_id).await {
        Ok(parents) => parents,
        Err(error) => {
            restore_stale_space_bookmarks(state, spaces_jid, channel_id, &removed_stale).await;
            return Err(error);
        }
    };
    for parent in parents.iter().filter(|parent| parent.as_str() != keep_node) {
        match delete_channel_parent_tuple(state, channel_id, parent).await {
            Ok(true) => removed_stale.push(RemovedStaleSpaceBookmark {
                node: parent.clone(),
                item: None,
                parent_tuple_deleted: true,
            }),
            Ok(false) => {}
            Err(error) => {
                restore_stale_space_bookmarks(state, spaces_jid, channel_id, &removed_stale).await;
                return Err(error);
            }
        }
    }
    Ok(())
}

struct RemovedStaleSpaceBookmark {
    node: String,
    item: Option<PubSubItem>,
    parent_tuple_deleted: bool,
}

async fn restore_stale_space_bookmarks(
    state: &WebSocketState,
    spaces_jid: &BareJid,
    channel_id: &str,
    removed_stale: &[RemovedStaleSpaceBookmark],
) {
    for removed in removed_stale.iter().rev() {
        let Some(item) = removed.item.as_ref() else {
            if removed.parent_tuple_deleted {
                let _ = write_channel_parent_tuple(state, channel_id, &removed.node).await;
            }
            continue;
        };
        match state
            .deps
            .protocol
            .pubsub_storage
            .publish_item(spaces_jid, &removed.node, item, None, false)
            .await
        {
            Ok(_) => {
                if removed.parent_tuple_deleted {
                    let _ = write_channel_parent_tuple(state, channel_id, &removed.node).await;
                }
            }
            Err(error) => {
                warn!(
                    channel_id = %channel_id,
                    node = %removed.node,
                    error = %error,
                    "Failed to restore stale Space bookmark after cleanup rollback"
                );
            }
        }
    }
}
