use super::permissions::{
    channel_parent_spaces, delete_channel_parent_tuple, server_permission_allowed,
    write_channel_parent_tuple, write_channel_parent_tuple_if_absent,
};
use super::spaces_bookmark_cleanup::cleanup_stale_space_bookmarks;
use super::{
    build_pubsub_error, build_pubsub_publish_result, chrono, get_xmpp_channel,
    spaces_node_mutation_allowed, spaces_service_bare_jid, warn, BareJid, Object, ObjectType,
    Permission, PubSubError, PubSubItem, WebSocketState,
};
use crate::channel_space_links::ChannelSpaceLink;
use crate::permissions::{ListRelations, Subject};
use crate::server::routes::websocket::handlers::pubsub_fanout;
use crate::server::routes::websocket::ResolvedPrincipal;
use crate::space_identity::{space_jid_for_node, SpaceNode};
use minidom::Element;
use waddle_xmpp::pubsub::NS_PUBSUB;
use waddle_xmpp::xep::xep0503::{space_image_payload, SpaceImage, SpaceImageError};

pub(super) async fn handle_spaces_publish(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    muc_domain: &str,
    spaces_domain: &str,
    node: &str,
    item: PubSubItem,
    principal: Option<ResolvedPrincipal<'_>>,
) -> Vec<xmpp_parsers::iq::Iq> {
    match spaces_node_mutation_allowed(state, principal, node).await {
        Ok(true) => {}
        Ok(false) => return vec![build_pubsub_error(iq, PubSubError::Forbidden)],
        Err(error) => {
            warn!(node, error = %error, "Failed to authorize Spaces publish");
            return vec![build_pubsub_error(iq, PubSubError::Forbidden)];
        }
    }
    let Ok(spaces_jid) = spaces_service_bare_jid(spaces_domain) else {
        return vec![build_pubsub_error(iq, PubSubError::InvalidJid)];
    };
    match state
        .deps
        .protocol
        .pubsub_storage
        .get_node(&spaces_jid, node)
        .await
    {
        Ok(Some(_)) => {}
        Ok(None) => return vec![build_pubsub_error(iq, PubSubError::NodeNotFound)],
        Err(error) => {
            warn!(node, error = %error, "Failed to resolve Spaces node for publish");
            return vec![build_pubsub_error(iq, PubSubError::NodeNotFound)];
        }
    }

    let Some(item_id) = item.id.as_deref() else {
        return vec![build_pubsub_error(iq, PubSubError::InvalidJid)];
    };
    if let Some(image) = SpaceImage::from_item_id(item_id) {
        return publish_space_image(iq, state, &spaces_jid, node, image).await;
    }
    let Some(payload) = item.payload.as_ref() else {
        return vec![build_pubsub_error(iq, PubSubError::InvalidJid)];
    };
    let bookmark = match waddle_xmpp::xep::xep0402::parse_bookmark(item_id, payload) {
        Ok(bookmark) => bookmark,
        Err(error) => {
            warn!(item_id, error = %error, "Invalid XEP-0402 Spaces item");
            return vec![build_pubsub_error(iq, PubSubError::InvalidJid)];
        }
    };
    if bookmark.jid.domain().as_str() != muc_domain {
        return vec![build_pubsub_error(iq, PubSubError::InvalidJid)];
    }
    let Some(channel_id) = waddle_xmpp::parse_managed_room_jid(&bookmark.jid) else {
        return vec![build_pubsub_error(iq, PubSubError::InvalidJid)];
    };
    let db_actor = state.deps.app_state.db_pool.global_actor().clone();
    match get_xmpp_channel(db_actor, &channel_id).await {
        Ok(Some(_)) => {}
        Ok(None) => return vec![build_pubsub_error(iq, PubSubError::ItemNotFound)],
        Err(error) => {
            warn!(channel_id = %channel_id, error = %error, "Failed to look up channel for Spaces bookmark");
            return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
        }
    }
    match channel_placement_allowed(
        state,
        principal,
        &spaces_jid,
        node,
        &bookmark.jid,
        &channel_id,
    )
    .await
    {
        Ok(true) => {}
        Ok(false) => return vec![build_pubsub_error(iq, PubSubError::Forbidden)],
        Err(error) => {
            warn!(error = %error, "Failed to authorize channel placement in a Space");
            return vec![build_pubsub_error(iq, PubSubError::Forbidden)];
        }
    }

    let stale_nodes = match state
        .deps
        .protocol
        .pubsub_storage
        .list_node_names_for_item(&spaces_jid, item_id)
        .await
    {
        Ok(stale_nodes) => stale_nodes,
        Err(error) => {
            warn!(
                channel_id = %channel_id,
                node,
                item_id,
                error = %error,
                "Failed to enumerate prior Space nodes for room bookmark"
            );
            return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
        }
    };
    let previous_link = match state
        .deps
        .app_state
        .channel_space_link_store
        .get(&bookmark.jid)
        .await
    {
        Ok(link) => link,
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to read channel-space link before Spaces publish");
            return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
        }
    };
    let current_item_filter = [item_id.to_string()];
    let previous_item = match state
        .deps
        .protocol
        .pubsub_storage
        .get_items(&spaces_jid, node, Some(1), &current_item_filter)
        .await
    {
        Ok(items) => items
            .into_iter()
            .next()
            .map(|stored| stored.to_pubsub_item()),
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to read existing Spaces item before publish");
            return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
        }
    };

    match state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(&spaces_jid, node, &item, None, false)
        .await
    {
        Ok(result) => {
            let space_node = SpaceNode::from(node);
            let projected_space_jid = space_jid_for_node(&spaces_jid, &space_node);
            if let Some(space_jid) = projected_space_jid.as_ref() {
                if let Err(error) = state
                    .deps
                    .app_state
                    .channel_space_link_store
                    .set(&ChannelSpaceLink {
                        channel_jid: bookmark.jid.clone(),
                        space_jid: space_jid.clone(),
                        space_node: space_node.clone(),
                        created_at: chrono::Utc::now().timestamp(),
                    })
                    .await
                {
                    warn!(
                        channel_id = %channel_id,
                        node,
                        error = %error,
                        "Published Spaces item but failed to sync channel-space link projection; \
                         rolling back to keep PubSub and durable discovery state consistent"
                    );
                    if let Err(rollback_error) = rollback_spaces_publish(
                        state,
                        SpacesPublishRollback {
                            spaces_jid: &spaces_jid,
                            node,
                            item_id: &result.item_id,
                            previous_item: previous_item.as_ref(),
                            channel_id: &channel_id,
                            channel_jid: &bookmark.jid,
                            previous_link: previous_link.as_ref(),
                            parent_tuple_created: false,
                        },
                    )
                    .await
                    {
                        warn!(
                            channel_id = %channel_id,
                            node,
                            error = %rollback_error,
                            "Failed to roll back Spaces publish after link projection failure"
                        );
                    }
                    return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
                }
            } else {
                warn!(
                    node,
                    spaces = %spaces_jid,
                    "Clearing channel-space link projection for non-JID Space node"
                );
                if let Err(error) = state
                    .deps
                    .app_state
                    .channel_space_link_store
                    .clear(&bookmark.jid)
                    .await
                {
                    warn!(
                        channel_id = %channel_id,
                        node,
                        error = %error,
                        "Published Spaces item but failed to clear stale channel-space link projection; \
                         rolling back to keep PubSub and durable discovery state consistent"
                    );
                    if let Err(rollback_error) = rollback_spaces_publish(
                        state,
                        SpacesPublishRollback {
                            spaces_jid: &spaces_jid,
                            node,
                            item_id: &result.item_id,
                            previous_item: previous_item.as_ref(),
                            channel_id: &channel_id,
                            channel_jid: &bookmark.jid,
                            previous_link: previous_link.as_ref(),
                            parent_tuple_created: false,
                        },
                    )
                    .await
                    {
                        warn!(
                            channel_id = %channel_id,
                            node,
                            error = %rollback_error,
                            "Failed to roll back Spaces publish after link projection clear failure"
                        );
                    }
                    return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
                }
            }
            let parent_tuple_created =
                match write_channel_parent_tuple_if_absent(state, &channel_id, node).await {
                    Ok(created) => created,
                    Err(error) => {
                        warn!(
                            channel_id = %channel_id,
                            node,
                            error = %error,
                            "Published Spaces item but failed to sync channel parent tuple; \
                             rolling back to keep PubSub and permission graph consistent"
                        );
                        if let Err(rollback_error) = rollback_spaces_publish(
                            state,
                            SpacesPublishRollback {
                                spaces_jid: &spaces_jid,
                                node,
                                item_id: &result.item_id,
                                previous_item: previous_item.as_ref(),
                                channel_id: &channel_id,
                                channel_jid: &bookmark.jid,
                                previous_link: previous_link.as_ref(),
                                parent_tuple_created: false,
                            },
                        )
                        .await
                        {
                            warn!(
                                channel_id = %channel_id,
                                node,
                                error = %rollback_error,
                                "Failed to roll back Spaces publish after parent tuple failure"
                            );
                        }
                        return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
                    }
                };
            // A placement moves the channel: every other Space stops being
            // its parent, whether or not that Space listed a bookmark of it.
            let moved_from = match cleanup_stale_space_bookmarks(
                state,
                &spaces_jid,
                &channel_id,
                node,
                item_id,
                &stale_nodes,
            )
            .await
            {
                Ok(moved_from) => moved_from,
                Err(error) => {
                    warn!(
                        channel_id = %channel_id,
                        node,
                        item_id,
                        error = %error,
                        "Published Spaces item but failed to clean stale prior Space bookmarks"
                    );
                    if let Err(rollback_error) = rollback_spaces_publish(
                        state,
                        SpacesPublishRollback {
                            spaces_jid: &spaces_jid,
                            node,
                            item_id: &result.item_id,
                            previous_item: previous_item.as_ref(),
                            channel_id: &channel_id,
                            channel_jid: &bookmark.jid,
                            previous_link: previous_link.as_ref(),
                            parent_tuple_created,
                        },
                    )
                    .await
                    {
                        warn!(
                            channel_id = %channel_id,
                            node,
                            item_id,
                            error = %rollback_error,
                            "Failed to roll back Spaces publish after stale cleanup failure"
                        );
                    }
                    return vec![build_pubsub_error(iq, PubSubError::InternalServerError)];
                }
            };
            // Fan-out only after the parent-tuple write succeeds: the
            // compensating-retract path above must NOT emit events for
            // a publish that gets rolled back.
            // Spaces publishes are owned by the spaces service domain,
            // not a user JID. `is_pep = false` skips the §3 roster +
            // owner-self passes (PR #439 review): the publisher's
            // roster has no authorization relationship to a Spaces
            // node, so running those passes would leak the event.
            pubsub_fanout::fan_out_publish(
                state,
                pubsub_fanout::FanOutRequest {
                    owner: &spaces_jid,
                    node,
                    published_item: &item,
                    item_id: &result.item_id,
                    publisher: None,
                    publisher_full: None,
                    is_pep: false,
                },
            )
            .await;
            // The Spaces the room moved out of tell their subscribers it
            // left them, as a retract of its bookmark there would.
            for stale in &moved_from {
                pubsub_fanout::fan_out_service_retract(
                    state,
                    pubsub_fanout::FanOutRetractRequest {
                        owner: &spaces_jid,
                        node: stale,
                        item_id,
                    },
                    false,
                )
                .await;
            }
            vec![build_pubsub_publish_result(iq, node, &result.item_id)]
        }
        Err(error) => {
            warn!(item_id, node, error = %error, "Failed to publish Spaces item");
            vec![build_pubsub_error(iq, PubSubError::InternalServerError)]
        }
    }
}

/// Publish the item of one of the Space's images (XEP-0503 0.3.0, "Spaces
/// images") to the Space `node`, for a publisher the caller has found may
/// change the Space.
///
/// The item is no room: it places no channel, writes no channel-space link
/// or parent relationship, and moves nothing out of another Space, where
/// an item under the same id is that Space's own image. Its payload is
/// checked as [`space_image_payload`] checks it, read from the request
/// itself so that a second payload element is refused rather than dropped,
/// and stored as given under the image's id, without the publisher the
/// request may name. Nothing the payload names is fetched. Subscribers are
/// notified as for any publication to the Space.
async fn publish_space_image(
    iq: &xmpp_parsers::iq::Iq,
    state: &WebSocketState,
    spaces_jid: &BareJid,
    node: &str,
    image: SpaceImage,
) -> Vec<xmpp_parsers::iq::Iq> {
    let Some(requested) = published_item(iq) else {
        return vec![build_pubsub_error(iq, PubSubError::BadRequest)];
    };
    let payload = match space_image_payload(requested) {
        Ok(payload) => payload.clone(),
        Err(SpaceImageError::MissingPayload) => {
            return vec![build_pubsub_error(iq, PubSubError::PayloadRequired)];
        }
        Err(error) => {
            warn!(node, item_id = image.item_id(), error = %error, "Invalid Space image item");
            return vec![build_pubsub_error(iq, PubSubError::InvalidPayload)];
        }
    };
    let item = PubSubItem::new(Some(image.item_id().to_owned()), Some(payload));
    match state
        .deps
        .protocol
        .pubsub_storage
        .publish_item(spaces_jid, node, &item, None, false)
        .await
    {
        Ok(result) => {
            pubsub_fanout::fan_out_publish(
                state,
                pubsub_fanout::FanOutRequest {
                    owner: spaces_jid,
                    node,
                    published_item: &item,
                    item_id: &result.item_id,
                    publisher: None,
                    publisher_full: None,
                    is_pep: false,
                },
            )
            .await;
            vec![build_pubsub_publish_result(iq, node, &result.item_id)]
        }
        Err(error) => {
            warn!(node, item_id = image.item_id(), error = %error, "Failed to publish Space image item");
            vec![build_pubsub_error(iq, PubSubError::InternalServerError)]
        }
    }
}

/// The `<item/>` of the `<publish/>` a PubSub set request carries, as the
/// request holds it.
fn published_item(iq: &xmpp_parsers::iq::Iq) -> Option<&Element> {
    let xmpp_parsers::iq::Iq::Set { payload, .. } = iq else {
        return None;
    };
    payload
        .get_child("publish", NS_PUBSUB)?
        .get_child("item", NS_PUBSUB)
}

/// Whether the publisher may place the existing channel `channel_id` in the
/// Space `node` of the Spaces service `spaces_jid`.
///
/// Authority over the Space node lets its owner list rooms in it, but
/// placing a channel in a Space also moves the channel's link to that
/// Space, makes the Space the channel's parent (so the Space's admins gain
/// the channel's `manage`, `moderate` and `delete`), and lets deleting the
/// Space destroy the channel. That is the channel's decision, taken here
/// from explicit relationships so that every permission backend decides it
/// alike:
///
/// - A publish that moves no authority is the Space's own: the channel
///   already belongs to this Space (its link or a parent relationship names
///   it) and to no other (neither names another Space). When the link and a
///   parent name different Spaces, the publish would move one of them, and
///   it needs the authority below.
/// - Otherwise the publisher holds explicit authority over the channel: an
///   owner, admin or manager relationship on the channel itself, or an owner
///   or admin relationship on a Space that is the channel's parent now (the
///   authority the channel inherits from its current Space), and no ban in
///   the channel; or the publisher is the deployment owner, who can already
///   place any channel through the channel administration commands.
///
/// A room an enrolled host-grant scope manages is placed by its canonical
/// topology only, never by a Spaces publish.
async fn channel_placement_allowed(
    state: &WebSocketState,
    principal: Option<ResolvedPrincipal<'_>>,
    spaces_jid: &BareJid,
    node: &str,
    channel_jid: &BareJid,
    channel_id: &str,
) -> Result<bool, String> {
    if state
        .deps
        .app_state
        .host_grants
        .channel_enrolled(channel_id)
        .await
        .map_err(|error| error.to_string())?
    {
        return Ok(false);
    }
    let link = state
        .deps
        .app_state
        .channel_space_link_store
        .get(channel_jid)
        .await
        .map_err(|error| format!("channel-space link lookup failed: {error}"))?;
    let parents = channel_parent_spaces(state, channel_id).await?;
    let link_here = link.as_ref().is_some_and(|link| {
        link.space_node.as_str() == node && link.space_jid.domain() == spaces_jid.domain()
    });
    let link_elsewhere = link.is_some() && !link_here;
    let parent_here = parents.iter().any(|parent| parent == node);
    let parent_elsewhere = parents.iter().any(|parent| parent != node);
    if (link_here || parent_here) && !link_elsewhere && !parent_elsewhere {
        return Ok(true);
    }
    if channel_authority(state, principal, channel_id, &parents).await? {
        return Ok(true);
    }
    server_permission_allowed(state, principal, Permission::Owner).await
}

/// Whether the publisher holds explicit authority over the channel: an
/// owner, admin or manager relationship on it, or an owner or admin
/// relationship on one of its current parent Spaces, and no ban in it.
async fn channel_authority(
    state: &WebSocketState,
    principal: Option<ResolvedPrincipal<'_>>,
    channel_id: &str,
    parents: &[String],
) -> Result<bool, String> {
    let Some(principal) = principal else {
        return Ok(false);
    };
    let relations = |object: Object| async move {
        state
            .deps
            .app_state
            .permission_actor
            .ask(ListRelations {
                subject: Subject::user(&principal.user_jid),
                object,
            })
            .await
            .map(|relations| {
                relations
                    .into_iter()
                    .map(|relation| relation.name)
                    .collect::<Vec<_>>()
            })
            .map_err(|error| format!("relationship lookup failed: {error}"))
    };
    let on_channel = relations(Object::new(ObjectType::Channel, channel_id)).await?;
    if on_channel.iter().any(|relation| relation == "outcast") {
        return Ok(false);
    }
    if on_channel
        .iter()
        .any(|relation| matches!(relation.as_str(), "owner" | "admin" | "manager"))
    {
        return Ok(true);
    }
    for parent in parents {
        let on_space = relations(Object::new(ObjectType::Space, parent.as_str())).await?;
        if on_space
            .iter()
            .any(|relation| matches!(relation.as_str(), "owner" | "admin"))
        {
            return Ok(true);
        }
    }
    Ok(false)
}

async fn restore_channel_space_link_projection(
    state: &WebSocketState,
    channel_jid: &BareJid,
    previous_link: Option<&ChannelSpaceLink>,
) -> Result<(), String> {
    if let Some(link) = previous_link {
        state
            .deps
            .app_state
            .channel_space_link_store
            .set(link)
            .await
            .map_err(|error| format!("channel-space link restore failed: {error}"))?;
    } else {
        state
            .deps
            .app_state
            .channel_space_link_store
            .clear(channel_jid)
            .await
            .map_err(|error| format!("channel-space link clear failed: {error}"))?;
    }
    Ok(())
}

struct SpacesPublishRollback<'a> {
    spaces_jid: &'a BareJid,
    node: &'a str,
    item_id: &'a str,
    previous_item: Option<&'a PubSubItem>,
    channel_id: &'a str,
    channel_jid: &'a BareJid,
    previous_link: Option<&'a ChannelSpaceLink>,
    parent_tuple_created: bool,
}

async fn rollback_spaces_publish(
    state: &WebSocketState,
    rollback: SpacesPublishRollback<'_>,
) -> Result<(), String> {
    if rollback.parent_tuple_created {
        delete_channel_parent_tuple(state, rollback.channel_id, rollback.node)
            .await
            .map_err(|error| format!("parent tuple rollback failed: {error}"))?;
    }
    if let Some(item) = rollback.previous_item {
        if let Err(error) = state
            .deps
            .protocol
            .pubsub_storage
            .publish_item(rollback.spaces_jid, rollback.node, item, None, false)
            .await
        {
            if rollback.parent_tuple_created {
                let _ = write_channel_parent_tuple(state, rollback.channel_id, rollback.node).await;
            }
            return Err(format!("pubsub restore Spaces item failed: {error}"));
        }
    } else {
        if let Err(error) = state
            .deps
            .protocol
            .pubsub_storage
            .retract_item(rollback.spaces_jid, rollback.node, rollback.item_id)
            .await
        {
            if rollback.parent_tuple_created {
                let _ = write_channel_parent_tuple(state, rollback.channel_id, rollback.node).await;
            }
            return Err(format!("pubsub rollback Spaces item failed: {error}"));
        }
    }
    restore_channel_space_link_projection(state, rollback.channel_jid, rollback.previous_link)
        .await?;
    Ok(())
}
