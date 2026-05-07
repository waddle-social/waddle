use crate::db::actor::{DbExecute, DbQueryOne};
use crate::server::AppState;
use crate::server::routes::websocket::XmppServiceDomains;
use anyhow::Result;
use jid::BareJid;
use std::sync::Arc;
use tracing::{info, warn};
use waddle_xmpp::pubsub::{NodeConfig, PubSubItem, PubSubStorage};

pub(crate) async fn bootstrap_fresh_xmpp_topology(
    state: &Arc<AppState>,
    pubsub_storage: Arc<dyn PubSubStorage>,
    services: &XmppServiceDomains,
) -> Result<()> {
    let spaces_jid: jid::BareJid = services
        .spaces
        .parse()
        .map_err(|error| anyhow::anyhow!("invalid spaces service JID: {error}"))?;

    let actor = state.db_pool.global_actor().clone();
    let row = actor
        .ask(DbQueryOne {
            sql: "SELECT COUNT(*) FROM channels".to_string(),
            params: vec![],
        })
        .await
        .map_err(|error| anyhow::anyhow!("failed to count channels: {error}"))?;
    let channel_count = row
        .as_ref()
        .and_then(|row| row.first())
        .and_then(|value| match value {
            crate::db::Value::Integer(value) => Some(*value),
            _ => None,
        })
        .unwrap_or(0);
    let should_seed_db = channel_count == 0;
    let should_seed_pubsub = should_seed_db
        || (actor
            .ask(DbQueryOne {
                sql: "SELECT 1 FROM channels WHERE id = 'chat'".to_string(),
                params: vec![],
            })
            .await
            .map_err(|error| anyhow::anyhow!("failed to inspect chat channel: {error}"))?
            .is_some()
            && actor
                .ask(DbQueryOne {
                    sql: "SELECT 1 FROM channels WHERE id = 'announcements'".to_string(),
                    params: vec![],
                })
                .await
                .map_err(|error| {
                    anyhow::anyhow!("failed to inspect announcements channel: {error}")
                })?
                .is_some());

    if should_seed_pubsub {
        seed_initial_xmpp_topology(
            &actor,
            &pubsub_storage,
            services,
            &spaces_jid,
            should_seed_db,
        )
        .await?;
    }

    seed_spaces_admin_affiliations(&pubsub_storage, &spaces_jid, &state.server_owner_jids).await;

    Ok(())
}

async fn seed_initial_xmpp_topology(
    actor: &kameo::actor::ActorRef<crate::db::actor::DbActor>,
    pubsub_storage: &Arc<dyn PubSubStorage>,
    services: &XmppServiceDomains,
    spaces_jid: &BareJid,
    should_seed_db: bool,
) -> Result<()> {
    if should_seed_db {
        let now = chrono::Utc::now().to_rfc3339();
        for (id, name, description, position, is_default, channel_type) in [
            ("chat", "Chat", "General member chat", 0_i64, 1_i64, "text"),
            (
                "announcements",
                "Announcements",
                "Owner-posted announcements",
                1_i64,
                0_i64,
                "announcement",
            ),
        ] {
            actor
                .ask(DbExecute {
                    sql: r#"
                        INSERT INTO channels (id, name, description, channel_type, position, is_default, created_at, updated_at)
                        VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                        ON CONFLICT(id) DO NOTHING
                    "#
                    .to_string(),
                    params: vec![
                        id.into(),
                        name.into(),
                        description.into(),
                        channel_type.into(),
                        position.into(),
                        is_default.into(),
                        now.clone().into(),
                        now.clone().into(),
                    ],
                })
                .await
                .map_err(|error| anyhow::anyhow!("failed to seed channel {id}: {error}"))?;
        }
    }

    pubsub_storage
        .get_or_create_node(spaces_jid, "general")
        .await
        .map_err(|error| anyhow::anyhow!("failed to create General space node: {error}"))?;
    pubsub_storage
        .update_node_config(spaces_jid, "general", &NodeConfig::spaces_public())
        .await
        .map_err(|error| anyhow::anyhow!("failed to configure General space node: {error}"))?;

    for (id, name) in [("chat", "Chat"), ("announcements", "Announcements")] {
        let room_jid = waddle_xmpp::managed_room_jid(id, &services.muc)
            .map_err(|error| anyhow::anyhow!("invalid seeded room JID: {error}"))?;
        let bookmark = waddle_xmpp::xep::xep0402::Bookmark::new(room_jid)
            .with_name(name)
            .with_autojoin(id == "chat");
        let item = PubSubItem {
            id: Some(bookmark.jid.to_string()),
            publisher: None,
            payload: Some(waddle_xmpp::xep::xep0402::build_bookmark_element(&bookmark)),
        };
        pubsub_storage
            .publish_item(spaces_jid, "general", &item, Some(spaces_jid), false)
            .await
            .map_err(|error| anyhow::anyhow!("failed to publish {name} bookmark: {error}"))?;
    }

    info!(
        muc = %services.muc,
        spaces = %services.spaces,
        "Seeded fresh XMPP General Space with Chat and Announcements MUCs"
    );
    Ok(())
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
