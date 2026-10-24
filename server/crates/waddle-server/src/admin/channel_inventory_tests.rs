//! Regression tests for durable admin inventory and filtered pagination.
use super::*;
use crate::db::{DatabaseConfig, DatabasePool, MigrationRunner, PoolConfig};
use waddle_xmpp::muc::durable::{ChannelId, WaddleId};
use waddle_xmpp::muc::room_actor::{Join, OccupancySessionGeneration};

async fn fresh_state() -> AppState {
    let pool = DatabasePool::try_new(DatabaseConfig::default(), PoolConfig)
        .await
        .expect("database pool");
    MigrationRunner::global()
        .run(pool.global())
        .await
        .expect("migrations");
    AppState::new(Arc::new(pool))
}

async fn catalog_channel(
    state: &AppState,
    id: &str,
    name: &str,
    channel_type: ChannelType,
    public: bool,
) -> BareJid {
    upsert_xmpp_channel(
        state.db_pool.global_actor().clone(),
        &XmppChannelUpsert {
            id: id.to_owned(),
            name: name.to_owned(),
            description: Some("Stored topic".to_owned()),
            channel_type: channel_type.as_str().to_owned(),
            position: 0,
            is_default: false,
            pin_permission: PinPermission::AdminsOnly,
            members_only: !public,
            public_room: public,
        },
    )
    .await
    .expect("catalog row");
    waddle_xmpp::managed_room_jid(id, state.muc_domain.as_str()).expect("room JID")
}

async fn list(state: &AppState, args: &ChannelsListArgs) -> ChannelsListResult {
    run_list(state, args)
        .await
        .unwrap_or_else(|_| panic!("channel inventory"))
}

fn first_page() -> ChannelsListArgs {
    ChannelsListArgs {
        page_size: 50,
        ..Default::default()
    }
}

#[tokio::test]
async fn dormant_catalog_channels_include_private_metadata_without_starting_rooms() {
    let state = fresh_state().await;
    let forum = catalog_channel(&state, "forum", "Stored forum", ChannelType::Forum, false).await;
    catalog_channel(
        &state,
        "group",
        "Private conversation",
        ChannelType::GroupDm,
        false,
    )
    .await;
    let result = list(&state, &first_page()).await;
    assert_eq!(result.entries.len(), 1);
    let entry = &result.entries[0];
    assert_eq!(entry.channel_jid, forum);
    assert_eq!(entry.name, "Stored forum");
    assert_eq!(entry.topic.as_deref(), Some("Stored topic"));
    assert_eq!(entry.channel_type, ChannelType::Forum);
    assert!(!entry.is_public);
    assert!(entry.members_only);
    assert_eq!(entry.occupant_count, 0);
    assert!(result.next_cursor.is_none());
    assert!(state
        .room_registry
        .ask(ListRooms)
        .await
        .expect("local rooms")
        .is_empty());
}

#[tokio::test]
async fn catalog_metadata_wins_while_live_counts_and_instant_rooms_are_preserved() {
    let state = fresh_state().await;
    let jid = catalog_channel(
        &state,
        "stored",
        "Canonical",
        ChannelType::Announcement,
        true,
    )
    .await;
    let owner: BareJid = "alice@localhost".parse().expect("owner");
    let actor = state
        .room_registry
        .ask(CreateRoomWithInitialAffiliations {
            room_jid: jid.clone(),
            waddle_id: WaddleId::new("test".to_owned()),
            channel_id: ChannelId::new("stored".to_owned()),
            config: RoomConfig {
                name: "Stale local name".to_owned(),
                ..Default::default()
            },
            initial_affiliations: vec![waddle_xmpp::muc::DurableAffiliationEntry::new(
                owner.clone(),
                Some(Affiliation::Owner),
            )],
        })
        .await
        .expect("live room");
    actor
        .ask(Join {
            session: OccupancySessionGeneration::mint(),
            nick: "alice".to_owned(),
            real_jid: "alice@localhost/web".parse().expect("full JID"),
            role: Role::Moderator,
            affiliation: Affiliation::Owner,
        })
        .await
        .expect("join");
    let instant: BareJid = "instant@muc.localhost".parse().expect("instant room");
    state
        .room_registry
        .ask(CreateRoom {
            room_jid: instant.clone(),
            waddle_id: "test".to_owned(),
            channel_id: "instant".to_owned(),
            config: RoomConfig {
                name: "Instant room".to_owned(),
                ..Default::default()
            },
        })
        .await
        .expect("instant actor");
    let result = list(&state, &first_page()).await;
    assert_eq!(
        result.entries.len(),
        2,
        "catalog/live duplicate appears only once"
    );
    let entry = result
        .entries
        .iter()
        .find(|entry| entry.channel_jid == jid)
        .expect("stored row");
    assert_eq!(entry.name, "Canonical");
    assert_eq!(entry.channel_type, ChannelType::Announcement);
    assert_eq!(entry.occupant_count, 1);
    assert_eq!(entry.affiliation_owner_count, 1);
    assert!(result
        .entries
        .iter()
        .any(|entry| entry.channel_jid == instant));
}

#[tokio::test]
async fn filtered_pagination_reads_past_catalog_batches_and_handles_exact_page_boundary() {
    let state = fresh_state().await;
    for index in 0..205 {
        catalog_channel(
            &state,
            &format!("filler-{index:03}"),
            "Other",
            ChannelType::Text,
            true,
        )
        .await;
    }
    for index in 0..50 {
        catalog_channel(
            &state,
            &format!("match-{index:03}"),
            "Match",
            ChannelType::Text,
            true,
        )
        .await;
    }
    let args = ChannelsListArgs {
        prefix: Some("match".to_owned()),
        ..first_page()
    };
    let exact = list(&state, &args).await;
    assert_eq!(exact.entries.len(), 50);
    assert!(
        exact.next_cursor.is_none(),
        "exactly one page has no continuation"
    );
    let last_jid = catalog_channel(&state, "match-050", "Match", ChannelType::Text, true).await;
    let first = list(&state, &args).await;
    assert_eq!(first.entries.len(), 50);
    let cursor = first
        .next_cursor
        .expect("51st matching channel produces continuation");
    assert_eq!(
        cursor,
        first
            .entries
            .last()
            .expect("last row")
            .channel_jid
            .to_string()
    );
    let second = list(
        &state,
        &ChannelsListArgs {
            after_cursor: Some(cursor),
            ..args
        },
    )
    .await;
    assert_eq!(second.entries.len(), 1);
    assert_eq!(second.entries[0].channel_jid, last_jid);
    assert!(second.next_cursor.is_none());
}

#[tokio::test]
async fn jid_cursor_order_and_unicode_literal_prefixes_remain_consistent() {
    let state = fresh_state().await;
    let a = catalog_channel(&state, "a", "Équipe_%", ChannelType::Text, true).await;
    let ab = catalog_channel(&state, "a-b", "Équipe_% second", ChannelType::Text, true).await;
    catalog_channel(&state, "other", "ÉquipeXX", ChannelType::Text, true).await;
    let args = ChannelsListArgs {
        prefix: Some("équipe_%".to_owned()),
        page_size: 1,
        ..Default::default()
    };
    let first = list(&state, &args).await;
    assert_eq!(first.entries.len(), 1);
    assert_eq!(first.entries[0].channel_jid, ab);
    let second = list(
        &state,
        &ChannelsListArgs {
            after_cursor: first.next_cursor,
            ..args
        },
    )
    .await;
    assert_eq!(second.entries.len(), 1);
    assert_eq!(second.entries[0].channel_jid, a);
    assert!(second.next_cursor.is_none());
}

#[tokio::test]
async fn exact_space_filter_includes_dormant_channels_before_page_limit() {
    let state = fresh_state().await;
    catalog_channel(&state, "a-unlinked", "Unlinked", ChannelType::Text, true).await;
    let one = catalog_channel(&state, "b-one", "One", ChannelType::Text, true).await;
    let two = catalog_channel(&state, "c-two", "Two", ChannelType::Text, true).await;
    let node = SpaceNode::from("Engineering");
    let other_node = SpaceNode::from("engineering");
    let space_jid =
        crate::space_identity::space_jid_for_node(&state.spaces_jid, &node).expect("space JID");
    let other_jid = crate::space_identity::space_jid_for_node(&state.spaces_jid, &other_node)
        .expect("other space JID");
    for (channel_jid, space_node, space_jid) in [
        (one.clone(), node.clone(), space_jid.clone()),
        (two, other_node, other_jid),
    ] {
        state
            .channel_space_link_store
            .set(&ChannelSpaceLink {
                channel_jid,
                space_jid,
                space_node,
                created_at: 0,
            })
            .await
            .expect("link");
    }
    let args = ChannelsListArgs {
        space_jid: Some(space_jid),
        space_node: Some(node.clone()),
        page_size: 1,
        ..Default::default()
    };
    let result = list(&state, &args).await;
    assert_eq!(result.entries.len(), 1);
    assert_eq!(result.entries[0].channel_jid, one);
    assert!(
        result.next_cursor.is_none(),
        "unrelated channels must not cause a continuation"
    );
    let node_only = list(
        &state,
        &ChannelsListArgs {
            space_node: Some(node),
            ..first_page()
        },
    )
    .await;
    assert_eq!(result.entries, node_only.entries);
}
