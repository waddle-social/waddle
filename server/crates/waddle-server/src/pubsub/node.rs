use jid::BareJid;
use waddle_xmpp::pubsub::{AccessModel, NodeConfig, PubSubNode};
use waddle_xmpp::XmppError;

use super::{
    item::{clear_bookmark_projection_tx, clear_dm_bookmark_projection_tx},
    DatabasePubSubStorage,
};

impl DatabasePubSubStorage {
    pub(super) async fn insert_node(&self, node: &PubSubNode) -> Result<(), XmppError> {
        let config = &node.config;
        self.execute(
            r#"
            INSERT INTO pubsub_nodes (
                owner_jid, node_name, access_model, publish_model, max_items,
                persist_items, deliver_payloads, notify_retract, notify_delete,
                send_last_published_item, node_type, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
            ON CONFLICT(owner_jid, node_name) DO NOTHING
            "#,
            crate::db_params![
                node.owner.to_string(),
                node.node_name.clone(),
                config.access_model.to_string(),
                config.publish_model.to_string(),
                config.max_items,
                config.persist_items,
                config.deliver_payloads,
                config.notify_retract,
                config.notify_delete,
                config.send_last_published_item.to_string(),
                config.node_type.map(|node_type| node_type.to_string()),
                node.created_at.timestamp_millis(),
            ],
        )
        .await?;
        Ok(())
    }

    pub(super) async fn get_or_create_node_impl(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<(PubSubNode, bool), XmppError> {
        if let Some(node) = self.get_node_impl(owner, node_name).await? {
            return Ok((node, false));
        }

        let node = PubSubNode::new_pep(owner.clone(), node_name.to_string());
        self.insert_node(&node).await?;
        Ok((node, true))
    }

    pub(super) async fn get_node_impl(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Option<PubSubNode>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT owner_jid, node_name, access_model, publish_model, max_items,
                       persist_items, deliver_payloads, notify_retract, notify_delete,
                       send_last_published_item, node_type, created_at_ms
                FROM pubsub_nodes
                WHERE owner_jid = ? AND node_name = ?
                "#,
                crate::db_params![owner.to_string(), node_name],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        else {
            return Ok(None);
        };
        let mut node = Self::decode_node(&row)?;

        // Lazy backfill (issue #1094): fan-out privacy is derived from
        // the PERSISTED access_model, so a well-known private node
        // created before its whitelist default existed (stored as
        // `presence`) would silently re-enter roster+CAPS fan-out. The
        // configure-time clamp in `bounded_node_config` only fires on
        // updates; heal legacy rows here on read so every consumer
        // (fan-out, authz, items) sees the pinned model. The UPDATE
        // runs once per stale row — steady state is a pure in-memory
        // comparison.
        if NodeConfig::pins_whitelist_access(node_name)
            && node.config.access_model != AccessModel::Whitelist
        {
            self.execute(
                "UPDATE pubsub_nodes SET access_model = ? WHERE owner_jid = ? AND node_name = ?",
                crate::db_params![
                    AccessModel::Whitelist.to_string(),
                    owner.to_string(),
                    node_name
                ],
            )
            .await?;
            node.config.access_model = AccessModel::Whitelist;
        }
        Ok(Some(node))
    }

    pub(super) async fn delete_node_impl(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<bool, XmppError> {
        // Deleting either notification-settings carrier MUST clear the
        // projection rows it fed in the same tx, scoped to that carrier's
        // source — otherwise the rows orphan (stale push suppression with
        // no wire way to clear them). The XEP-0402 MUC node clears
        // `Xep0402Bookmarks` rows; the Waddle DM node clears
        // `WaddleDmBookmarks` rows. Keying on the source leaves the other
        // carrier's rows untouched (mirrors the purge path).
        if is_bookmarks_node(node_name) || is_dm_bookmarks_node(node_name) {
            let is_dm = is_dm_bookmarks_node(node_name);
            let mut tx = self
                .db
                .begin()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            let affected = tx
                .execute(
                    "DELETE FROM pubsub_nodes WHERE owner_jid = ? AND node_name = ?",
                    crate::db_params![owner.to_string(), node_name],
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            if affected > 0 {
                if is_dm {
                    clear_dm_bookmark_projection_tx(&mut tx, owner).await?;
                } else {
                    clear_bookmark_projection_tx(&mut tx, owner).await?;
                }
            }
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(affected > 0);
        }

        let affected = self
            .execute(
                "DELETE FROM pubsub_nodes WHERE owner_jid = ? AND node_name = ?",
                crate::db_params![owner.to_string(), node_name],
            )
            .await?;
        Ok(affected > 0)
    }

    pub(super) async fn list_nodes_impl(&self, owner: &BareJid) -> Result<Vec<String>, XmppError> {
        let mut rows = self
            .query(
                "SELECT node_name FROM pubsub_nodes WHERE owner_jid = ? ORDER BY node_name ASC",
                crate::db_params![owner.to_string()],
            )
            .await?;
        let mut nodes = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            nodes.push(
                row.get(0)
                    .map_err(|error| XmppError::internal(error.to_string()))?,
            );
        }
        Ok(nodes)
    }

    pub(super) async fn find_node_for_item_impl(
        &self,
        owner: &BareJid,
        item_id: &str,
    ) -> Result<Option<PubSubNode>, XmppError> {
        // When the same item_id exists in multiple nodes (e.g. a
        // XEP-0503 room bookmark briefly duplicated across two
        // Spaces during a move), the prior `ORDER BY n.node_name
        // ASC` would return the alphabetically-first node, which
        // had nothing to do with the user's most recent intent.
        // Concretely it pinned room bookmarks under `general` even
        // after the client published the bookmark to another space.
        //
        // Order by the items table's auto-increment `seq` DESC so
        // the most recent publish wins. `list_node_names_for_item`
        // exists for the rare case where the caller actually wants
        // every membership (the publish path uses it to retract
        // older duplicates).
        let mut rows = self
            .query(
                r#"
                SELECT n.owner_jid, n.node_name, n.access_model, n.publish_model, n.max_items,
                       n.persist_items, n.deliver_payloads, n.notify_retract, n.notify_delete,
                       n.send_last_published_item, n.node_type, n.created_at_ms
                FROM pubsub_nodes n
                JOIN pubsub_items i
                  ON i.owner_jid = n.owner_jid AND i.node_name = n.node_name
                WHERE n.owner_jid = ? AND i.item_id = ?
                ORDER BY i.seq DESC
                LIMIT 1
                "#,
                crate::db_params![owner.to_string(), item_id],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(Self::decode_node(&row)?))
    }

    /// Return the names of every node owned by `owner` that contains an
    /// item with the given `item_id`. Used by single-membership enforcement
    /// paths (XEP-0503 channel→space pinning) to identify stale duplicates
    /// before a publish.
    pub(super) async fn list_node_names_for_item_impl(
        &self,
        owner: &BareJid,
        item_id: &str,
    ) -> Result<Vec<String>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT node_name
                FROM pubsub_items
                WHERE owner_jid = ? AND item_id = ?
                "#,
                crate::db_params![owner.to_string(), item_id],
            )
            .await?;
        let mut names = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            names.push(
                row.get(0)
                    .map_err(|error| XmppError::internal(error.to_string()))?,
            );
        }
        Ok(names)
    }

    pub(super) async fn update_node_config_impl(
        &self,
        owner: &BareJid,
        node_name: &str,
        config: &NodeConfig,
    ) -> Result<(), XmppError> {
        let config = bounded_node_config(node_name, config);
        let affected = self
            .execute(
                r#"
                UPDATE pubsub_nodes
                SET access_model = ?,
                    publish_model = ?,
                    max_items = ?,
                    persist_items = ?,
                    deliver_payloads = ?,
                    notify_retract = ?,
                    notify_delete = ?,
                    send_last_published_item = ?,
                    node_type = ?
                WHERE owner_jid = ? AND node_name = ?
                "#,
                crate::db_params![
                    config.access_model.to_string(),
                    config.publish_model.to_string(),
                    config.max_items,
                    config.persist_items,
                    config.deliver_payloads,
                    config.notify_retract,
                    config.notify_delete,
                    config.send_last_published_item.to_string(),
                    config.node_type.map(|node_type| node_type.to_string()),
                    owner.to_string(),
                    node_name,
                ],
            )
            .await?;
        if affected == 0 {
            return Err(XmppError::item_not_found(Some(format!(
                "Node '{node_name}' does not exist"
            ))));
        }
        Ok(())
    }

    fn decode_node(row: &crate::db::Row) -> Result<PubSubNode, XmppError> {
        let owner_raw: String = row
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let owner = owner_raw
            .parse::<BareJid>()
            .map_err(|error| XmppError::internal(format!("invalid PubSub owner JID: {error}")))?;
        let node_name: String = row
            .get(1)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let access_model_raw: String = row
            .get(2)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let publish_model_raw: String = row
            .get(3)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let max_items: i64 = row
            .get(4)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let persist_items: bool = row
            .get(5)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let deliver_payloads: bool = row
            .get(6)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let notify_retract: bool = row
            .get(7)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let notify_delete: bool = row
            .get(8)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let send_last_raw: String = row
            .get(9)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let node_type_raw: Option<String> = row
            .get(10)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let node_type = node_type_raw
            .as_deref()
            .map(str::parse)
            .transpose()
            .map_err(|_| XmppError::internal("invalid PubSub node_type".to_string()))?;
        let created_at_ms: i64 = row
            .get(11)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let created_at = chrono::DateTime::from_timestamp_millis(created_at_ms)
            .ok_or_else(|| XmppError::internal("invalid PubSub created_at_ms".to_string()))?;

        Ok(PubSubNode {
            node_name,
            owner,
            config: NodeConfig {
                access_model: access_model_raw.parse().unwrap_or_default(),
                publish_model: publish_model_raw.parse().unwrap_or_default(),
                node_type,
                max_items: u32::try_from(max_items)
                    .map_err(|error| XmppError::internal(error.to_string()))?,
                persist_items,
                deliver_payloads,
                notify_retract,
                notify_delete,
                send_last_published_item: send_last_raw.parse().unwrap_or_default(),
            },
            created_at,
        })
    }
}

fn bounded_node_config(node_name: &str, config: &NodeConfig) -> NodeConfig {
    if is_bookmarks_node(node_name) {
        return config.clone().normalize_xep0402_bookmarks();
    }
    if is_dm_bookmarks_node(node_name) {
        return config.clone().normalize_waddle_dm_bookmarks();
    }
    let mut config = config.clone();
    // Well-known private PEP nodes (whitelist in `NodeConfig::pep_for_node`:
    // MDS per XEP-0490 §3, DND, story reads, status preference) must stay
    // whitelist: an owner configure-set flipping the access model would
    // re-enable the #1094 roster fan-out and non-owner item reads. Same
    // predicate as the read-path backfill in `get_node_impl`.
    if NodeConfig::pins_whitelist_access(node_name) {
        config.access_model = AccessModel::Whitelist;
    }
    config
}

fn is_bookmarks_node(node_name: &str) -> bool {
    node_name == waddle_xmpp::xep::xep0402::PEP_NODE
}

fn is_dm_bookmarks_node(node_name: &str) -> bool {
    waddle_xmpp::xep::xep_waddle_dm_bookmarks::is_dm_bookmarks_node(node_name)
}
