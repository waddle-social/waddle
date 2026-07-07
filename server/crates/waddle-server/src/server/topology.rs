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
    members_only: bool,
    public_room: bool,
}

const INITIAL_MANAGED_CHANNELS: &[ManagedChannelSeed] = &[
    ManagedChannelSeed {
        id: "chat",
        name: "Chat",
        description: "General member chat",
        position: 0,
        is_default: 1,
        channel_type: "text",
        members_only: false,
        public_room: true,
    },
    ManagedChannelSeed {
        id: "announcements",
        name: "Announcements",
        description: "Owner-posted announcements",
        position: 1,
        is_default: 0,
        channel_type: "announcement",
        members_only: false,
        public_room: true,
    },
    ManagedChannelSeed {
        id: "github-actions",
        name: "GitHub Actions",
        description: "GitHub Actions alerts",
        position: 2,
        is_default: 0,
        channel_type: "text",
        members_only: false,
        public_room: true,
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
                    INSERT INTO channels (
                        id, name, description, channel_type, position, is_default,
                        members_only, public_room, created_at, updated_at
                    )
                    VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
                    ON CONFLICT(id) DO UPDATE SET
                        channel_type = excluded.channel_type,
                        members_only = excluded.members_only,
                        public_room = excluded.public_room,
                        updated_at = excluded.updated_at
                "#
                .to_string(),
                params: vec![
                    channel.id.into(),
                    channel.name.into(),
                    channel.description.into(),
                    channel.channel_type.into(),
                    channel.position.into(),
                    channel.is_default.into(),
                    channel.members_only.into(),
                    channel.public_room.into(),
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
    // `community_feed()`: anyone can subscribe and read, and any
    // authenticated member may publish (`publish_model: Open`), per
    // XEP-0472 §"Replying to a Post". Stories use the same member-postable
    // access shape below; calendar keeps `spaces_public()` plus an RSVP carve-out.
    pubsub_storage
        .get_or_create_node(&community_jid, waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED)
        .await
        .map_err(|error| anyhow::anyhow!("failed to create social feed node: {error}"))?;
    pubsub_storage
        .update_node_config(
            &community_jid,
            waddle_xmpp_core::xep0472::PUBSUB_NODE_FEED,
            &NodeConfig::community_feed(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to configure social feed node: {error}"))?;

    // XEP-0501 Stories — community-wide ephemeral pubsub node. Any
    // authenticated, non-outcast member may publish; the service stamps
    // the authenticated author into the payload before storing it.
    // Stories carry an `expires` timestamp and the chat client filters
    // expired items out of the view.
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
            &NodeConfig::community_stories(),
        )
        .await
        .map_err(|error| anyhow::anyhow!("failed to configure stories node: {error}"))?;

    // xCal Proto-calendar events — community-wide PubSub node for
    // events with optional RSVP tracking. Calendar event creation stays
    // owner-only; per-attendee RSVP items have a separate authenticated
    // member carve-out in the IQ handler.
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
        match room_registry
            .ask(GetOrCreateRoom {
                room_jid: room_jid.clone(),
                waddle_id: if channel_record.channel_type
                    == waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM
                {
                    waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM.to_string()
                } else {
                    "space".to_string()
                },
                channel_id: channel_record.id.clone(),
                config: RoomConfig {
                    name: channel_record.name.clone(),
                    description: channel_record.description.clone(),
                    members_only: channel_record.members_only,
                    public_room: channel_record.public_room,
                    moderated: channel_record.channel_type == "announcement",
                    forum: channel_record.channel_type == "forum",
                    group_dm: channel_record.channel_type
                        == waddle_xmpp::admin::CHANNEL_TYPE_GROUP_DM,
                    pin_permission: channel_record.pin_permission,
                    ..Default::default()
                },
            })
            .await
        {
            Ok(_) => {}
            // ADR-0017 Phase 3 Slice 7 FIX 5 (council-adjudicated): another
            // node genuinely, currently owns this room's claim — a normal
            // steady-state condition once any node in the cluster has
            // already seeded topology, NOT a startup failure. Log and
            // continue seeding the REST of the channels/spaces/bookmarks;
            // previously this `?` aborted the entire function (and, via
            // its own caller's `?`, `seed_spaces_admin_affiliations` too)
            // on the very first already-claimed-elsewhere room.
            Err(kameo::error::SendError::HandlerError(
                waddle_xmpp::muc::room_registry_actor::RoomRegistryError::ClaimHeldByAnotherNode(_),
            )) => {
                tracing::info!(
                    channel = %channel_record.id,
                    room = %room_jid,
                    "topology seed: room's ownership claim is held by another live node; \
                     owned elsewhere, continuing with the rest of the seed"
                );
            }
            Err(error) => {
                return Err(anyhow::anyhow!(
                    "failed to create managed room actor for {}: {error}",
                    channel_record.id
                ));
            }
        }
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
    use super::{
        bootstrap_fresh_xmpp_topology, AppState, XmppServiceDomains, INITIAL_MANAGED_CHANNELS,
    };
    use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
    use std::sync::Arc;
    use waddle_xmpp::muc::room_registry_actor::WireClusteringClaims;
    use waddle_xmpp::ownership::{
        ClaimStore, Entity, EntityType, InProcessClaimStore, NodeIdentity, SharedNodeIdentity,
    };

    fn test_services() -> XmppServiceDomains {
        XmppServiceDomains {
            muc: "muc.example.com".to_string(),
            spaces: "spaces@pubsub.example.com".to_string(),
            upload: "upload.example.com".to_string(),
            extensions: "extensions.example.com".to_string(),
            push: "push.example.com".to_string(),
            community: "community.example.com".to_string(),
        }
    }

    /// ADR-0017 Phase 3 Slice 7 FIX 5 (council-adjudicated): a room
    /// genuinely, currently claimed by another live node is a normal
    /// steady-state condition (any node after the first to seed topology
    /// hits this for every already-seeded room), NOT a startup failure —
    /// the rest of the seed (other channels, spaces admin affiliations,
    /// bookmarks backfill) must still complete.
    #[tokio::test]
    async fn bootstrap_completes_the_rest_of_the_seed_when_one_room_is_claimed_elsewhere() {
        let db_pool = DatabasePool::new(DatabaseConfig::default(), PoolConfig)
            .await
            .expect("db pool");
        MigrationRunner::global()
            .run(db_pool.global())
            .await
            .expect("migrations");
        let state = Arc::new(AppState::new(Arc::new(db_pool)));
        let services = test_services();

        // Pre-claim the "chat" managed channel's room under a foreign,
        // live node identity BEFORE bootstrap runs — models "another node
        // already seeded/owns this room."
        let claim_store: Arc<dyn ClaimStore> = Arc::new(InProcessClaimStore::new());
        let foreign = NodeIdentity::new("foreign-node", "foreign-epoch");
        let chat_room_jid =
            waddle_xmpp::managed_room_jid("chat", &services.muc).expect("valid room jid");
        let chat_entity = Entity::new(EntityType::RoomActor, chat_room_jid.to_string());
        claim_store
            .acquire(&chat_entity, &foreign)
            .await
            .expect("foreign pre-claim");

        state
            .room_registry
            .ask(WireClusteringClaims {
                claim_store: Arc::clone(&claim_store),
                node_identity: SharedNodeIdentity::new(NodeIdentity::new(
                    "this-node",
                    "this-epoch",
                )),
                durable_store: None,
            })
            .await
            .expect("wire clustering claims");

        let pubsub_storage = state.pubsub_storage.clone();
        let room_registry = state.room_registry.clone();
        let result =
            bootstrap_fresh_xmpp_topology(&state, pubsub_storage, &services, &room_registry).await;
        assert!(
            result.is_ok(),
            "bootstrap must complete despite one room being claimed elsewhere: {result:?}"
        );

        // A DIFFERENT channel's room must still have been created —
        // proving the loop continued past the claimed-elsewhere room
        // instead of aborting the whole function.
        let github_room_jid =
            waddle_xmpp::managed_room_jid("github-actions", &services.muc).expect("valid room jid");
        let github_entity = Entity::new(EntityType::RoomActor, github_room_jid.to_string());
        assert!(
            claim_store
                .current_claim(&github_entity)
                .await
                .expect("current_claim")
                .is_some(),
            "a non-claimed-elsewhere channel's room must still have been created"
        );

        // The claimed-elsewhere room's claim must be untouched — still
        // the foreign node's, never silently reassigned.
        let chat_claim = claim_store
            .current_claim(&chat_entity)
            .await
            .expect("current_claim")
            .expect("still claimed");
        assert_eq!(chat_claim.owner, foreign);
    }

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

    #[test]
    fn initial_managed_channels_are_public_and_open() {
        for channel in INITIAL_MANAGED_CHANNELS {
            assert!(
                !channel.members_only,
                "seeded channel {} must be open",
                channel.id
            );
            assert!(
                channel.public_room,
                "seeded channel {} must be public",
                channel.id
            );
        }
    }
}
