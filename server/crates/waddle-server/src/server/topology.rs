use crate::db::actor::{DbActor, DbExecute};
use crate::permissions::{
    Object, ObjectType, PermissionError, Relation, Subject, SubjectType, Tuple, WriteTuple,
};
use crate::server::routes::websocket::XmppServiceDomains;
use crate::server::xmpp_channels::get_xmpp_channel;
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
        write_channel_parent_tuple(state, channel.id, "general").await?;
    }

    pubsub_storage
        .get_or_create_node(spaces_jid, "general")
        .await
        .map_err(|error| anyhow::anyhow!("failed to create General space node: {error}"))?;
    pubsub_storage
        .update_node_config(spaces_jid, "general", &NodeConfig::spaces_public())
        .await
        .map_err(|error| anyhow::anyhow!("failed to configure General space node: {error}"))?;

    for channel in INITIAL_MANAGED_CHANNELS {
        let channel_record = get_xmpp_channel(actor.clone(), channel.id)
            .await
            .map_err(|error| anyhow::anyhow!("failed to load channel {}: {error}", channel.id))?
            .ok_or_else(|| anyhow::anyhow!("seeded channel {} is missing", channel.id))?;
        let room_jid = waddle_xmpp::managed_room_jid(channel.id, &services.muc)
            .map_err(|error| anyhow::anyhow!("invalid seeded room JID: {error}"))?;
        room_registry
            .ask(GetOrCreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: "space".to_string(),
                channel_id: channel_record.id.clone(),
                config: RoomConfig {
                    name: channel_record.name.clone(),
                    description: channel_record.description.clone(),
                    members_only: true,
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
                    channel.id
                )
            })?;
        let item_id = room_jid.to_string();
        if pubsub_storage
            .get_items(
                spaces_jid,
                "general",
                Some(1),
                std::slice::from_ref(&item_id),
            )
            .await
            .map_err(|error| {
                anyhow::anyhow!("failed to inspect {} bookmark: {error}", channel.name)
            })?
            .is_empty()
        {
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
