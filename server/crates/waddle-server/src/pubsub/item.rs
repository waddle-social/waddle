use jid::BareJid;
use waddle_xmpp::pubsub::{PubSubItem, PubSubNode, PublishResult, StoredItem};
use waddle_xmpp::XmppError;

use super::DatabasePubSubStorage;

impl DatabasePubSubStorage {
    pub(super) async fn publish_item_impl(
        &self,
        owner: &BareJid,
        node_name: &str,
        item: &PubSubItem,
        publisher: Option<&BareJid>,
        auto_create: bool,
    ) -> Result<PublishResult, XmppError> {
        let (node, node_created) = match self.get_node_impl(owner, node_name).await? {
            Some(node) => (node, false),
            None if auto_create => {
                let node = PubSubNode::new_pep(owner.clone(), node_name.to_string());
                self.insert_node(&node).await?;
                (node, true)
            }
            None => {
                return Err(XmppError::item_not_found(Some(format!(
                    "Node '{node_name}' does not exist"
                ))));
            }
        };

        let item_id = item
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let payload_xml = item.payload.as_ref().map(String::from);
        let publisher_jid = publisher.map(ToString::to_string);
        let published_at_ms = crate::time::now_ms();

        self.execute(
            r#"
            INSERT INTO pubsub_items (
                owner_jid, node_name, item_id, payload_xml, publisher_jid, published_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(owner_jid, node_name, item_id) DO UPDATE SET
                payload_xml = excluded.payload_xml,
                publisher_jid = excluded.publisher_jid,
                published_at_ms = excluded.published_at_ms
            "#,
            crate::db_params![
                owner.to_string(),
                node_name,
                item_id.clone(),
                payload_xml,
                publisher_jid,
                published_at_ms,
            ],
        )
        .await?;
        self.enforce_max_items(owner, node_name, node.config.max_items)
            .await?;

        Ok(PublishResult {
            item_id,
            node_created,
        })
    }

    pub(super) async fn get_items_impl(
        &self,
        owner: &BareJid,
        node_name: &str,
        max_items: Option<u32>,
        item_ids: &[String],
    ) -> Result<Vec<StoredItem>, XmppError> {
        if !item_ids.is_empty() {
            // Build IN (?, ?, ...) clause inline. item_ids comes from a parsed
            // IQ payload and is bounded by the request size. Use placeholders;
            // never string-format the values.
            let placeholders = std::iter::repeat_n("?", item_ids.len())
                .collect::<Vec<_>>()
                .join(",");
            let sql = format!(
                r#"
                SELECT item_id, payload_xml, publisher_jid, published_at_ms
                FROM pubsub_items
                WHERE owner_jid = ? AND node_name = ? AND item_id IN ({placeholders})
                ORDER BY seq ASC
                "#
            );
            let mut params: Vec<crate::db::Value> = Vec::with_capacity(2 + item_ids.len());
            params.push(crate::db::Value::from(owner.to_string()));
            params.push(crate::db::Value::from(node_name));
            for id in item_ids {
                params.push(crate::db::Value::from(id.clone()));
            }
            return self.run_select_items(&sql, params).await;
        }

        // Bug 4 fix: use a parameterized LIMIT instead of string-formatting the
        // integer into the SQL text. Value::from(i64) is supported by the db layer.
        let limit_value: Option<i64> = match max_items {
            Some(n) if n > 0 => Some(n as i64),
            _ => None,
        };

        let sql = if limit_value.is_some() {
            r#"
            SELECT item_id, payload_xml, publisher_jid, published_at_ms FROM (
                SELECT item_id, payload_xml, publisher_jid, published_at_ms, seq
                FROM pubsub_items
                WHERE owner_jid = ? AND node_name = ?
                ORDER BY seq DESC
                LIMIT ?
            ) t
            ORDER BY seq ASC
            "#
        } else {
            r#"
            SELECT item_id, payload_xml, publisher_jid, published_at_ms
            FROM pubsub_items
            WHERE owner_jid = ? AND node_name = ?
            ORDER BY seq ASC
            "#
        };

        let mut params: Vec<crate::db::Value> = vec![
            crate::db::Value::from(owner.to_string()),
            crate::db::Value::from(node_name),
        ];
        if let Some(n) = limit_value {
            params.push(crate::db::Value::from(n));
        }
        self.run_select_items(sql, params).await
    }

    pub(super) async fn retract_item_impl(
        &self,
        owner: &BareJid,
        node_name: &str,
        item_id: &str,
    ) -> Result<bool, XmppError> {
        let affected = self
            .execute(
                "DELETE FROM pubsub_items WHERE owner_jid = ? AND node_name = ? AND item_id = ?",
                crate::db_params![owner.to_string(), node_name, item_id],
            )
            .await?;
        Ok(affected > 0)
    }

    pub(super) async fn purge_node_impl(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<u64, XmppError> {
        let affected = self
            .execute(
                "DELETE FROM pubsub_items WHERE owner_jid = ? AND node_name = ?",
                crate::db_params![owner.to_string(), node_name],
            )
            .await?;
        Ok(affected)
    }

    async fn enforce_max_items(
        &self,
        owner: &BareJid,
        node_name: &str,
        max_items: u32,
    ) -> Result<(), XmppError> {
        if max_items == 0 || max_items == u32::MAX {
            return Ok(());
        }
        self.execute(
            r#"
            DELETE FROM pubsub_items
            WHERE owner_jid = ? AND node_name = ?
              AND seq NOT IN (
                  SELECT seq FROM pubsub_items
                  WHERE owner_jid = ? AND node_name = ?
                  ORDER BY seq DESC
                  LIMIT ?
              )
            "#,
            crate::db_params![
                owner.to_string(),
                node_name,
                owner.to_string(),
                node_name,
                max_items,
            ],
        )
        .await?;
        Ok(())
    }

    async fn run_select_items(
        &self,
        sql: &str,
        params: Vec<crate::db::Value>,
    ) -> Result<Vec<StoredItem>, XmppError> {
        let mut rows = self.query(sql, params).await?;
        let mut items = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            items.push(Self::decode_item(&row)?);
        }
        Ok(items)
    }

    fn decode_item(row: &crate::db::Row) -> Result<StoredItem, XmppError> {
        let id: String = row
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let payload_xml: Option<String> = row
            .get(1)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let publisher_raw: Option<String> = row
            .get(2)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let published_at_ms: i64 = row
            .get(3)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let publisher = publisher_raw
            .map(|raw| {
                raw.parse::<BareJid>().map_err(|error| {
                    XmppError::internal(format!("invalid PubSub publisher JID: {error}"))
                })
            })
            .transpose()?;
        let published_at = chrono::DateTime::from_timestamp_millis(published_at_ms)
            .ok_or_else(|| XmppError::internal("invalid PubSub published_at_ms".to_string()))?;
        Ok(StoredItem {
            id,
            payload_xml,
            publisher,
            published_at,
        })
    }
}
