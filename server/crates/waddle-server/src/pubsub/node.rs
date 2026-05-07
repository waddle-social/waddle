use jid::BareJid;
use waddle_xmpp::XmppError;
use waddle_xmpp::pubsub::{NodeConfig, PubSubNode};

use super::DatabasePubSubStorage;

impl DatabasePubSubStorage {
    pub(super) async fn insert_node(&self, node: &PubSubNode) -> Result<(), XmppError> {
        let config = &node.config;
        self.execute(
            r#"
            INSERT INTO pubsub_nodes (
                owner_jid, node_name, access_model, publish_model, max_items,
                persist_items, deliver_payloads, notify_retract, notify_delete,
                send_last_published_item, created_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)
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
                       send_last_published_item, created_at_ms
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
        Ok(Some(Self::decode_node(&row)?))
    }

    pub(super) async fn delete_node_impl(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<bool, XmppError> {
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
        let mut rows = self
            .query(
                r#"
                SELECT n.owner_jid, n.node_name, n.access_model, n.publish_model, n.max_items,
                       n.persist_items, n.deliver_payloads, n.notify_retract, n.notify_delete,
                       n.send_last_published_item, n.created_at_ms
                FROM pubsub_nodes n
                JOIN pubsub_items i
                  ON i.owner_jid = n.owner_jid AND i.node_name = n.node_name
                WHERE n.owner_jid = ? AND i.item_id = ?
                ORDER BY n.node_name ASC
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

    pub(super) async fn update_node_config_impl(
        &self,
        owner: &BareJid,
        node_name: &str,
        config: &NodeConfig,
    ) -> Result<(), XmppError> {
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
                    send_last_published_item = ?
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
        let created_at_ms: i64 = row
            .get(10)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let created_at = chrono::DateTime::from_timestamp_millis(created_at_ms)
            .ok_or_else(|| XmppError::internal("invalid PubSub created_at_ms".to_string()))?;

        Ok(PubSubNode {
            node_name,
            owner,
            config: NodeConfig {
                access_model: access_model_raw.parse().unwrap_or_default(),
                publish_model: publish_model_raw.parse().unwrap_or_default(),
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
