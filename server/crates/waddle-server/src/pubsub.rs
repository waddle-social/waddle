//! Database-backed storage for XEP-0060 PubSub and XEP-0163 PEP data.

use async_trait::async_trait;
use jid::{BareJid, Jid};
use waddle_xmpp::XmppError;
use waddle_xmpp::pubsub::{
    Affiliation, NodeConfig, PubSubItem, PubSubNode, PubSubStorage, StoredItem, SubId,
    Subscription, SubscriptionState,
};

use crate::db::Database;

mod database;
mod open;
mod schema;

pub use open::build_pubsub_storage;

#[derive(Clone)]
pub struct DatabasePubSubStorage {
    db: Database,
}

impl DatabasePubSubStorage {
    async fn insert_node(&self, node: &PubSubNode) -> Result<(), XmppError> {
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
}

#[async_trait]
impl PubSubStorage for DatabasePubSubStorage {
    async fn get_or_create_node(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<(PubSubNode, bool), XmppError> {
        if let Some(node) = self.get_node(owner, node_name).await? {
            return Ok((node, false));
        }

        let node = PubSubNode::new_pep(owner.clone(), node_name.to_string());
        self.insert_node(&node).await?;
        Ok((node, true))
    }

    async fn get_node(
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

    async fn delete_node(&self, owner: &BareJid, node_name: &str) -> Result<bool, XmppError> {
        let affected = self
            .execute(
                "DELETE FROM pubsub_nodes WHERE owner_jid = ? AND node_name = ?",
                crate::db_params![owner.to_string(), node_name],
            )
            .await?;
        Ok(affected > 0)
    }

    async fn publish_item(
        &self,
        owner: &BareJid,
        node_name: &str,
        item: &PubSubItem,
        publisher: Option<&BareJid>,
        auto_create: bool,
    ) -> Result<waddle_xmpp::pubsub::PublishResult, XmppError> {
        let (node, node_created) = match self.get_node(owner, node_name).await? {
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

        Ok(waddle_xmpp::pubsub::PublishResult {
            item_id,
            node_created,
        })
    }

    async fn get_items(
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

    async fn retract_item(
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

    async fn list_nodes(&self, owner: &BareJid) -> Result<Vec<String>, XmppError> {
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

    async fn find_node_for_item(
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

    async fn update_node_config(
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

    async fn purge_node(&self, owner: &BareJid, node_name: &str) -> Result<u64, XmppError> {
        let affected = self
            .execute(
                "DELETE FROM pubsub_items WHERE owner_jid = ? AND node_name = ?",
                crate::db_params![owner.to_string(), node_name],
            )
            .await?;
        Ok(affected)
    }

    async fn subscribe(
        &self,
        owner: &BareJid,
        node_name: &str,
        subscriber: &Jid,
    ) -> Result<Subscription, XmppError> {
        // Normalizes subscriber JID to bare; full-JID subscriptions are not currently
        // supported in the DB store, matching the in-memory behavior used by
        // `list_deliverable_subscribers`. This prevents outcast-filter bypass when a
        // subscription arrives with a resource but the affiliation is stored bare.
        let subscriber_bare = subscriber.to_bare();
        let subid = SubId::generate();
        let now = crate::time::now_ms();
        self.execute(
            r#"
            INSERT INTO pubsub_subscriptions (owner_jid, node_name, subid, subscriber_jid, state, created_at_ms)
            VALUES (?, ?, ?, ?, 'subscribed', ?)
            "#,
            crate::db_params![
                owner.to_string(),
                node_name,
                subid.as_str().to_string(),
                subscriber_bare.to_string(),
                now,
            ],
        )
        .await?;
        Ok(Subscription {
            subid,
            subscriber: Jid::from(subscriber_bare),
            state: SubscriptionState::Subscribed,
            created_at_ms: now,
        })
    }

    async fn unsubscribe(
        &self,
        owner: &BareJid,
        node_name: &str,
        subscriber: &Jid,
        subid: Option<&SubId>,
    ) -> Result<bool, XmppError> {
        // Always normalise to bare JID so the lookup matches the bare-normalised
        // value written by `subscribe`.
        let subscriber_bare = subscriber.to_bare().to_string();
        let affected = match subid {
            Some(subid) => {
                self.execute(
                    "DELETE FROM pubsub_subscriptions WHERE owner_jid = ? AND node_name = ? AND subid = ? AND subscriber_jid = ?",
                    crate::db_params![
                        owner.to_string(),
                        node_name,
                        subid.as_str().to_string(),
                        subscriber_bare,
                    ],
                )
                .await?
            }
            None => {
                // Bug 2 fix: when no subid is provided, select the single oldest
                // matching subscription and delete only that row. This matches the
                // in-memory contract (remove at most one subscription per call) and
                // prevents accidentally removing multiple subscriptions for the same
                // (owner, node, subscriber) triple when multi-sub is in use.
                let mut rows = self
                    .query(
                        "SELECT subid FROM pubsub_subscriptions WHERE owner_jid = ? AND node_name = ? AND subscriber_jid = ? ORDER BY created_at_ms ASC, subid ASC LIMIT 1",
                        crate::db_params![
                            owner.to_string(),
                            node_name,
                            subscriber_bare.clone(),
                        ],
                    )
                    .await?;
                let Some(row) = rows
                    .next()
                    .await
                    .map_err(|e| XmppError::internal(e.to_string()))?
                else {
                    return Ok(false);
                };
                let found_subid: String =
                    row.get(0).map_err(|e| XmppError::internal(e.to_string()))?;
                self.execute(
                    "DELETE FROM pubsub_subscriptions WHERE owner_jid = ? AND node_name = ? AND subid = ?",
                    crate::db_params![owner.to_string(), node_name, found_subid],
                )
                .await?
            }
        };
        Ok(affected > 0)
    }

    async fn list_node_subscriptions(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<Subscription>, XmppError> {
        let mut rows = self
            .query(
                "SELECT subid, subscriber_jid, state, created_at_ms FROM pubsub_subscriptions WHERE owner_jid = ? AND node_name = ? ORDER BY created_at_ms ASC",
                crate::db_params![owner.to_string(), node_name],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| XmppError::internal(e.to_string()))?
        {
            out.push(decode_subscription(&row)?);
        }
        Ok(out)
    }

    async fn list_subscriber_subscriptions(
        &self,
        owner: &BareJid,
        subscriber: &Jid,
    ) -> Result<Vec<(String, Subscription)>, XmppError> {
        let mut rows = self
            .query(
                "SELECT node_name, subid, subscriber_jid, state, created_at_ms FROM pubsub_subscriptions WHERE owner_jid = ? AND subscriber_jid = ? ORDER BY node_name ASC, created_at_ms ASC",
                crate::db_params![owner.to_string(), subscriber.to_bare().to_string()],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| XmppError::internal(e.to_string()))?
        {
            let node: String = row.get(0).map_err(|e| XmppError::internal(e.to_string()))?;
            let sub = decode_subscription_offset(&row, 1)?;
            out.push((node, sub));
        }
        Ok(out)
    }

    async fn get_subscription(
        &self,
        owner: &BareJid,
        node_name: &str,
        subid: &SubId,
    ) -> Result<Option<Subscription>, XmppError> {
        let mut rows = self
            .query(
                "SELECT subid, subscriber_jid, state, created_at_ms FROM pubsub_subscriptions WHERE owner_jid = ? AND node_name = ? AND subid = ?",
                crate::db_params![owner.to_string(), node_name, subid.as_str().to_string()],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|e| XmppError::internal(e.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_subscription(&row)?))
    }

    async fn list_deliverable_subscribers(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<Subscription>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT s.subid, s.subscriber_jid, s.state, s.created_at_ms
                FROM pubsub_subscriptions s
                LEFT JOIN pubsub_affiliations a
                  ON a.owner_jid = s.owner_jid
                 AND a.node_name = s.node_name
                 AND a.entity_jid = s.subscriber_jid
                WHERE s.owner_jid = ?
                  AND s.node_name = ?
                  AND s.state = 'subscribed'
                  AND (a.affiliation IS NULL OR a.affiliation <> 'outcast')
                "#,
                crate::db_params![owner.to_string(), node_name],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| XmppError::internal(e.to_string()))?
        {
            out.push(decode_subscription(&row)?);
        }
        Ok(out)
    }

    async fn set_affiliation(
        &self,
        owner: &BareJid,
        node_name: &str,
        entity: &BareJid,
        affiliation: Affiliation,
    ) -> Result<Affiliation, XmppError> {
        let prev = self.get_affiliation(owner, node_name, entity).await?;
        if affiliation == Affiliation::None {
            self.execute(
                "DELETE FROM pubsub_affiliations WHERE owner_jid = ? AND node_name = ? AND entity_jid = ?",
                crate::db_params![owner.to_string(), node_name, entity.to_string()],
            )
            .await?;
            return Ok(prev);
        }
        self.execute(
            r#"
            INSERT INTO pubsub_affiliations (owner_jid, node_name, entity_jid, affiliation, updated_at_ms)
            VALUES (?, ?, ?, ?, ?)
            ON CONFLICT(owner_jid, node_name, entity_jid) DO UPDATE SET
                affiliation = excluded.affiliation,
                updated_at_ms = excluded.updated_at_ms
            "#,
            crate::db_params![
                owner.to_string(),
                node_name,
                entity.to_string(),
                affiliation.to_string(),
                crate::time::now_ms(),
            ],
        )
        .await?;
        Ok(prev)
    }

    async fn get_affiliation(
        &self,
        owner: &BareJid,
        node_name: &str,
        entity: &BareJid,
    ) -> Result<Affiliation, XmppError> {
        let mut rows = self
            .query(
                "SELECT affiliation FROM pubsub_affiliations WHERE owner_jid = ? AND node_name = ? AND entity_jid = ?",
                crate::db_params![owner.to_string(), node_name, entity.to_string()],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|e| XmppError::internal(e.to_string()))?
        else {
            return Ok(Affiliation::None);
        };
        let raw: String = row.get(0).map_err(|e| XmppError::internal(e.to_string()))?;
        Ok(raw.parse().unwrap_or(Affiliation::None))
    }

    async fn list_node_affiliations(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Vec<(BareJid, Affiliation)>, XmppError> {
        let mut rows = self
            .query(
                "SELECT entity_jid, affiliation FROM pubsub_affiliations WHERE owner_jid = ? AND node_name = ? ORDER BY entity_jid ASC",
                crate::db_params![owner.to_string(), node_name],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| XmppError::internal(e.to_string()))?
        {
            let entity_raw: String = row.get(0).map_err(|e| XmppError::internal(e.to_string()))?;
            let entity = entity_raw
                .parse::<BareJid>()
                .map_err(|e| XmppError::internal(e.to_string()))?;
            let aff_raw: String = row.get(1).map_err(|e| XmppError::internal(e.to_string()))?;
            let aff: Affiliation = aff_raw.parse().unwrap_or(Affiliation::None);
            out.push((entity, aff));
        }
        Ok(out)
    }

    async fn list_entity_affiliations(
        &self,
        owner: &BareJid,
        entity: &BareJid,
    ) -> Result<Vec<(String, Affiliation)>, XmppError> {
        let mut rows = self
            .query(
                "SELECT node_name, affiliation FROM pubsub_affiliations WHERE owner_jid = ? AND entity_jid = ? ORDER BY node_name ASC",
                crate::db_params![owner.to_string(), entity.to_string()],
            )
            .await?;
        let mut out = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|e| XmppError::internal(e.to_string()))?
        {
            let node: String = row.get(0).map_err(|e| XmppError::internal(e.to_string()))?;
            let aff_raw: String = row.get(1).map_err(|e| XmppError::internal(e.to_string()))?;
            let aff: Affiliation = aff_raw.parse().unwrap_or(Affiliation::None);
            out.push((node, aff));
        }
        Ok(out)
    }
}

fn decode_subscription(row: &crate::db::Row) -> Result<Subscription, XmppError> {
    decode_subscription_offset(row, 0)
}

fn decode_subscription_offset(
    row: &crate::db::Row,
    offset: usize,
) -> Result<Subscription, XmppError> {
    let subid_raw: String = row
        .get(offset)
        .map_err(|e| XmppError::internal(e.to_string()))?;
    let subscriber_raw: String = row
        .get(offset + 1)
        .map_err(|e| XmppError::internal(e.to_string()))?;
    let state_raw: String = row
        .get(offset + 2)
        .map_err(|e| XmppError::internal(e.to_string()))?;
    let created_at_ms: i64 = row
        .get(offset + 3)
        .map_err(|e| XmppError::internal(e.to_string()))?;
    Ok(Subscription {
        subid: SubId::from_raw(subid_raw),
        subscriber: subscriber_raw
            .parse::<Jid>()
            .map_err(|e| XmppError::internal(format!("invalid subscriber JID: {e}")))?,
        state: state_raw.parse().unwrap_or(SubscriptionState::Subscribed),
        created_at_ms,
    })
}

#[cfg(test)]
mod tests;
