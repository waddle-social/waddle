use crate::db::actor::{DbActor, DbExecute};
use crate::permissions::{
    Object, ObjectType, PermissionError, Relation, Subject, SubjectType, Tuple, WriteTuple,
};
use crate::server::routes::websocket::XmppServiceDomains;
use crate::server::xmpp_channels::{get_xmpp_channel, list_xmpp_channels};
use crate::server::AppState;
use anyhow::Result;
use jid::BareJid;
use kameo::actor::ActorRef;
use std::sync::Arc;
use tracing::{info, warn};
use waddle_xmpp::muc::{
    room_registry_actor::{GetOrCreateRoom, RoomRegistryActor},
    RoomConfig,
};
use waddle_xmpp::pubsub::{NodeConfig, PubSubItem, PubSubStorage};

struct ManagedChannelSeed {
    id: &'static str,
    name: &'static str,
    description: &'static str,
    position: i64,
    is_default: i64,
    channel_type: &'static str,
}

const INITIAL_MANAGED_CHANNELS: &[ManagedChannelSeed] = &[
    ManagedChannelSeed {
        id: "chat",
        name: "Chat",
        description: "General member chat",
        position: 0,
        is_default: 1,
        channel_type: "text",
    },
    ManagedChannelSeed {
        id: "announcements",
        name: "Announcements",
        description: "Owner-posted announcements",
        position: 1,
        is_default: 0,
        channel_type: "announcement",
    },
    ManagedChannelSeed {
        id: "github-actions",
        name: "GitHub Actions",
        description: "GitHub Actions alerts",
        position: 2,
        is_default: 0,
        channel_type: "text",
    },
];

pub(crate) async fn bootstrap_fresh_xmpp_topology(
    state: &Arc<AppState>,
    pubsub_storage: Arc<dyn PubSubStorage>,
    services: &XmppServiceDomains,
    room_registry: &ActorRef<RoomRegistryActor>,
) -> Result<()> {
    let spaces_jid: jid::BareJid = services
        .spaces
        .parse()
        .map_err(|error| anyhow::anyhow!("invalid spaces service JID: {error}"))?;

    let actor = state.db_pool.global_actor().clone();
    seed_initial_xmpp_topology(
        &actor,
        state,
        &pubsub_storage,
        services,
        &spaces_jid,
        room_registry,
    )
    .await?;

    seed_spaces_admin_affiliations(&pubsub_storage, &spaces_jid, &state.server_owner_jids).await;

    Ok(())
}

async fn seed_initial_xmpp_topology(
    actor: &ActorRef<DbActor>,
    state: &Arc<AppState>,
    pubsub_storage: &Arc<dyn PubSubStorage>,
    services: &XmppServiceDomains,
    spaces_jid: &BareJid,
    room_registry: &ActorRef<RoomRegistryActor>,
) -> Result<()> {
    let now = chrono::Utc::now().to_rfc3339();
    for channel in INITIAL_MANAGED_CHANNELS {
        actor
            .ask(DbExecute {
                sql: r#"
                    INSERT INTO channels (id, name, description, channel_type, position, is_default, created_at, updated_at)
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT(id) DO NOTHING
                "#
                .to_string(),
                params: vec![
                    channel.id.into(),
                    channel.name.into(),
                    channel.description.into(),
                    channel.channel_type.into(),
                    channel.position.into(),
                    channel.is_default.into(),
                    now.clone().into(),
                    now.clone().into(),
                ],
            })
            .await
            .map_err(|error| anyhow::anyhow!("failed to seed channel {}: {error}", channel.id))?;
    }

    pubsub_storage
        .get_or_create_node(spaces_jid, "general")
        .await
        .map_err(|error| anyhow::anyhow!("failed to create General space node: {error}"))?;
    pubsub_storage
        .update_node_config(spaces_jid, "general", &NodeConfig::spaces_public())
        .await
        .map_err(|error| anyhow::anyhow!("failed to configure General space node: {error}"))?;

    // Community service hosts the non-space pubsub nodes (XEP-0472
    // social feed, XEP-0501 stories). Kept distinct from
    // `spaces.<domain>` so the spaces disco#items enumeration cleanly
    // returns only actual community spaces.
    let community_jid: jid::BareJid = services
        .community
        .parse()
        .map_err(|error| anyhow::anyhow!("invalid community service JID: {error}"))?;

    // XEP-0472 Social Feed — community-wide pubsub feed for
    // announcements and microblog-style posts. Access model is
    // `spaces_public()`: anyone can subscribe and read, only entities
    // with Publisher or Owner affiliation may publish.
    pubsub_storage
        .get_or_create_node(&community_jid, waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED)
        .await
        .map_err(|error| anyhow::anyhow!("failed to create social feed node: {error}"))?;
    pubsub_storage
        .update_node_config(
            &community_jid,
            waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED,
            &NodeConfig::spaces_public(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to configure social feed node: {error}"))?;

    // XEP-0501 Stories — community-wide ephemeral pubsub node. Same
    // hosting + access model as the social feed; stories carry an
    // `expires` timestamp and the chat client filters expired items
    // out of the view. (Server-side eviction of expired items is a
    // future cleanup job; the items API and pubsub semantics work
    // unmodified — the `expires` attribute is application-level
    // metadata.)
    pubsub_storage
        .get_or_create_node(
            &community_jid,
            waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES,
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to create stories node: {error}"))?;
    pubsub_storage
        .update_node_config(
            &community_jid,
            waddle_xmpp_core::xep0501::PUBSUB_NODE_STORIES,
            &NodeConfig::spaces_public(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to configure stories node: {error}"))?;

    // XEP-0471 Calendar Events — community-wide pubsub node for
    // events with optional RSVP tracking. Same hosting + access
    // model as the social feed; events carry their own scheduling
    // metadata in the typed `<event/>` payload.
    pubsub_storage
        .get_or_create_node(&community_jid, waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS)
        .await
        .map_err(|error| anyhow::anyhow!("failed to create calendar node: {error}"))?;
    pubsub_storage
        .update_node_config(
            &community_jid,
            waddle_xmpp_core::xcal::PUBSUB_NODE_EVENTS,
            &NodeConfig::spaces_public(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to configure calendar node: {error}"))?;

    // Server owners get Owner affiliation on the community nodes so
    // the existing publisher gates accept their publishes. Mirrors
    // the spaces seed below but scoped to the community service.
    for owner in state.server_owner_jids.iter() {
        crate::spaces_pubsub_seed::seed_owner_on_all_nodes(pubsub_storage, &community_jid, owner)
            .await
            .map_err(|error| {
                anyhow::anyhow!("failed to seed community service owner affiliations: {error}")
            })?;
    }

    let persisted_channels = list_xmpp_channels(actor.clone(), 10_000, 0)
        .await
        .map_err(|error| anyhow::anyhow!("failed to load persisted channels: {error}"))?;
    for channel_record in &persisted_channels {
        let room_jid = waddle_xmpp::managed_room_jid(&channel_record.id, &services.muc)
            .map_err(|error| anyhow::anyhow!("invalid seeded room JID: {error}"))?;
        room_registry
            .ask(GetOrCreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "space".to_string(),
                channel_id: channel_record.id.clone(),
                config: RoomConfig {
                    name: channel_record.name.clone(),
                    description: channel_record.description.clone(),
                    members_only: channel_record.members_only,
                    public_room: channel_record.public_room,
                    moderated: channel_record.channel_type == "announcement",
                    forum: channel_record.channel_type == "forum",
                    pin_permission: channel_record.pin_permission,
                    ..Default::default()
                },
            })
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "failed to create managed room actor for {}: {error}",
                    channel_record.id
                )
            })?;
    }

    for channel in INITIAL_MANAGED_CHANNELS {
        let channel_record = get_xmpp_channel(actor.clone(), channel.id)
            .await
            .map_err(|error| anyhow::anyhow!("failed to load channel {}: {error}", channel.id))?
            .ok_or_else(|| anyhow::anyhow!("seeded channel {} is missing", channel.id))?;
        let room_jid = waddle_xmpp::managed_room_jid(channel.id, &services.muc)
            .map_err(|error| anyhow::anyhow!("invalid seeded room JID: {error}"))?;
        let item_id = room_jid.to_string();
        // XEP-0503 single-space-membership: only seed the channel
        // into General if it's not already pinned to ANY space.
        // The prior `get_items(general, ...)` check was scoped to
        // General alone, so re-running the seed against a channel
        // an admin had moved to another Space would re-add it to
        // General, leaving the room in two Spaces and pinning it
        // under General via the alphabetical `find_node_for_item`
        // tiebreak.
        let existing_space = pubsub_storage
            .list_node_names_for_item(spaces_jid, &item_id)
            .await
            .map_err(|error| {
                anyhow::anyhow!(
                    "failed to inspect {} space membership: {error}",
                    channel.name
                )
            })?;
        if existing_space.is_empty() {
            let bookmark = waddle_xmpp::xep::xep0402::Bookmark::new(room_jid)
                .with_name(channel_record.name)
                .with_autojoin(channel.id == "chat");
            let item = PubSubItem {
                id: Some(bookmark.jid.to_string()),
                publisher: None,
                payload: Some(waddle_xmpp::xep::xep0402::build_bookmark_element(&bookmark)),
            };
            pubsub_storage
                .publish_item(spaces_jid, "general", &item, Some(spaces_jid), false)
                .await
                .map_err(|error| {
                    anyhow::anyhow!("failed to publish {} bookmark: {error}", channel.name)
                })?;
            write_channel_parent_tuple(state, channel.id, "general").await?;
        }
    }

    info!(
        muc = %services.muc,
        spaces = %services.spaces,
        "Seeded baseline XMPP General Space managed MUCs"
    );
    Ok(())
}

async fn write_channel_parent_tuple(
    state: &Arc<AppState>,
    channel_id: &str,
    space_node: &str,
) -> Result<()> {
    let tuple = Tuple::new(
        Object::new(ObjectType::Channel, channel_id),
        Relation::new("parent"),
        Subject::userset(SubjectType::Space, space_node, ""),
    );
    match state.permission_actor.ask(WriteTuple { tuple }).await {
        Ok(())
        | Err(kameo::error::SendError::HandlerError(PermissionError::TupleAlreadyExists)) => Ok(()),
        Err(error) => Err(anyhow::anyhow!(
            "failed to write channel parent tuple for {channel_id}: {error}"
        )),
    }
}

/// Mirror server-owner permissions into `Affiliation::Owner` rows on every
/// existing Spaces PubSub node so XEP-0060 admin operations
/// (`<configure/>`, `<purge/>`, `<affiliations/>`) succeed for accounts in
/// `WADDLE_SERVER_OWNER_LOCALPARTS`. Per-entity failures are logged and do
/// not abort the batch.
async fn seed_spaces_admin_affiliations(
    pubsub_storage: &Arc<dyn PubSubStorage>,
    spaces_jid: &BareJid,
    server_owner_jids: &[BareJid],
) {
    if server_owner_jids.is_empty() {
        return;
    }
    let nodes = match pubsub_storage.list_nodes(spaces_jid).await {
        Ok(nodes) => nodes,
        Err(error) => {
            warn!(
                spaces = %spaces_jid,
                error = %error,
                "failed to enumerate Spaces nodes for server-owner affiliation seed",
            );
            return;
        }
    };
    for node in &nodes {
        crate::spaces_pubsub_seed::seed_owners_on_node(
            pubsub_storage,
            spaces_jid,
            node,
            server_owner_jids,
        )
        .await;
    }
}

#[cfg(test)]
mod tests {
    use super::INITIAL_MANAGED_CHANNELS;

    #[test]
    fn initial_managed_channels_include_github_actions_alert_room() {
        let channel = INITIAL_MANAGED_CHANNELS
            .iter()
            .find(|channel| channel.id == "github-actions")
            .expect("github-actions managed channel is seeded");

        let room_jid = waddle_xmpp::managed_room_jid(channel.id, "muc.waddle.social")
            .expect("managed room jid is valid");

        assert_eq!(channel.name, "GitHub Actions");
        assert_eq!(room_jid.to_string(), "github-actions@muc.waddle.social");
    }
}
