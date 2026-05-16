//! Database-backed storage for XEP-0060 PubSub and XEP-0163 PEP data.

use async_trait::async_trait;
use jid::{BareJid, Jid};
use waddle_xmpp::pubsub::{
    Affiliation, NodeConfig, PubSubItem, PubSubNode, PubSubStorage, StoredItem, SubId,
    Subscription, SubscriptionState,
};
use waddle_xmpp::XmppError;

use crate::db::Database;

mod database;
mod item;
mod node;
mod open;
mod schema;

pub use open::{build_database_pubsub_storage, build_pubsub_storage};

#[derive(Clone)]
pub struct DatabasePubSubStorage {
    db: Database,
}

#[async_trait]
impl PubSubStorage for DatabasePubSubStorage {
    async fn get_or_create_node(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<(PubSubNode, bool), XmppError> {
        self.get_or_create_node_impl(owner, node_name).await
    }

    async fn get_node(
        &self,
        owner: &BareJid,
        node_name: &str,
    ) -> Result<Option<PubSubNode>, XmppError> {
        self.get_node_impl(owner, node_name).await
    }

    async fn delete_node(&self, owner: &BareJid, node_name: &str) -> Result<bool, XmppError> {
        self.delete_node_impl(owner, node_name).await
    }

    async fn publish_item(
        &self,
        owner: &BareJid,
        node_name: &str,
        item: &PubSubItem,
        publisher: Option<&BareJid>,
        auto_create: bool,
    ) -> Result<waddle_xmpp::pubsub::PublishResult, XmppError> {
        self.publish_item_impl(owner, node_name, item, publisher, auto_create)
            .await
    }

    async fn get_items(
        &self,
        owner: &BareJid,
        node_name: &str,
        max_items: Option<u32>,
        item_ids: &[String],
    ) -> Result<Vec<StoredItem>, XmppError> {
        self.get_items_impl(owner, node_name, max_items, item_ids)
            .await
    }

    async fn retract_item(
        &self,
        owner: &BareJid,
        node_name: &str,
        item_id: &str,
    ) -> Result<bool, XmppError> {
        self.retract_item_impl(owner, node_name, item_id).await
    }

    async fn list_nodes(&self, owner: &BareJid) -> Result<Vec<String>, XmppError> {
        self.list_nodes_impl(owner).await
    }

    async fn find_node_for_item(
        &self,
        owner: &BareJid,
        item_id: &str,
    ) -> Result<Option<PubSubNode>, XmppError> {
        self.find_node_for_item_impl(owner, item_id).await
    }

    async fn list_node_names_for_item(
        &self,
        owner: &BareJid,
        item_id: &str,
    ) -> Result<Vec<String>, XmppError> {
        self.list_node_names_for_item_impl(owner, item_id).await
    }

    async fn update_node_config(
        &self,
        owner: &BareJid,
        node_name: &str,
        config: &NodeConfig,
    ) -> Result<(), XmppError> {
        self.update_node_config_impl(owner, node_name, config).await
    }

    async fn purge_node(&self, owner: &BareJid, node_name: &str) -> Result<u64, XmppError> {
        self.purge_node_impl(owner, node_name).await
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
