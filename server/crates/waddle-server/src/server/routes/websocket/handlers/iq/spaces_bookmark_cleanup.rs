use super::permissions::{delete_channel_parent_tuple, write_channel_parent_tuple};
use super::*;

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
        if let Some(item) = removed.item.as_ref() {
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
}
