use jid::BareJid;
use tracing::warn;
use waddle_xmpp::pubsub::{PubSubItem, PubSubNode, PublishResult, StoredItem};
use waddle_xmpp::XmppError;

use super::DatabasePubSubStorage;
use crate::dnd_projection::{delete_dnd_projection_tx, upsert_dnd_projection_tx, DndProjection};
use crate::notification_settings_projection::{
    derive_validated_bookmark_projection_mutation, validate_xep0402_bookmark_publish,
    ConversationKind, NotificationSettingsProjectionMutation,
};

impl DatabasePubSubStorage {
    pub(super) async fn publish_item_impl(
        &self,
        owner: &BareJid,
        node_name: &str,
        item: &PubSubItem,
        publisher: Option<&BareJid>,
        auto_create: bool,
    ) -> Result<PublishResult, XmppError> {
        let item_id = item
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let validated_bookmark = if is_bookmarks_node(node_name) {
            let payload = item.payload.as_ref().ok_or_else(|| {
                XmppError::bad_request(Some(
                    "XEP-0402 bookmark publish requires a conference payload".to_string(),
                ))
            })?;
            Some(
                validate_xep0402_bookmark_publish(&item_id, payload)
                    .map_err(|error| XmppError::bad_request(Some(error.to_string())))?,
            )
        } else {
            None
        };
        // Validate `urn:waddle:dnd:0` publishes up-front so the wire
        // publish gets a `<bad-request/>` instead of leaving the PEP
        // item in place with a stale projection alongside.
        //
        // Restrict publishes to the case where the publisher IS the
        // owner. DND is user-identity state — even if the user grants
        // explicit Publisher affiliation to a peer for some other PEP
        // feature, the peer must NOT be able to write `urn:waddle:dnd:0`
        // items. Allowing the wire-level publish-only while skipping
        // the projection write would leave `pubsub_items` and the
        // projection in disagreement; subscribers would see the peer's
        // spoofed payload while the T1 push gate consults the owner's
        // last-known state. Better to reject outright with
        // `<forbidden/>`.
        let dnd_publish_owner = if is_dnd_node(node_name) {
            match publisher {
                Some(publisher_jid) if publisher_jid == owner => {}
                _ => {
                    warn!(
                        owner = %owner,
                        publisher = ?publisher,
                        "urn:waddle:dnd:0 publish rejected: publisher is not the node owner"
                    );
                    return Err(XmppError::forbidden(Some(
                        "urn:waddle:dnd:0 may only be published by the node owner".to_string(),
                    )));
                }
            }
            // Enforce the XEP-0163 single-item PEP convention
            // (`id="current"`). Without this, a client that publishes
            // with `id="custom"` then later retracts `id="current"`
            // would silently leave the projection in place — the user
            // would stay in DND with no wire-level way to clear it.
            // Reject up-front so the client surfaces the mistake.
            match item.id.as_deref() {
                Some(id) if id == waddle_xmpp::xep::xep_waddle_dnd::ITEM_ID_CURRENT => {}
                _ => {
                    warn!(
                        owner = %owner,
                        item_id = ?item.id,
                        "urn:waddle:dnd:0 publish rejected: item id is not 'current'"
                    );
                    return Err(XmppError::bad_request(Some(format!(
                        "urn:waddle:dnd:0 publish must use item id '{}'",
                        waddle_xmpp::xep::xep_waddle_dnd::ITEM_ID_CURRENT,
                    ))));
                }
            }
            let payload = item.payload.as_ref().ok_or_else(|| {
                warn!(
                    owner = %owner,
                    "urn:waddle:dnd:0 publish rejected: missing <dnd> payload"
                );
                XmppError::bad_request(Some(
                    "urn:waddle:dnd:0 publish requires a <dnd> payload".to_string(),
                ))
            })?;
            let parsed =
                waddle_xmpp::xep::xep_waddle_dnd::WaddleDnd::parse(payload).map_err(|error| {
                    warn!(
                        owner = %owner,
                        %error,
                        "urn:waddle:dnd:0 publish rejected: invalid <dnd> payload"
                    );
                    XmppError::bad_request(Some(error.to_string()))
                })?;
            Some((owner.clone(), parsed))
        } else {
            None
        };

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

        let payload_xml = item.payload.as_ref().map(String::from);
        let publisher_jid = publisher.map(ToString::to_string);
        let published_at_ms = crate::time::now_ms();

        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;

        tx.execute(
            r#"
            INSERT INTO pubsub_items (
                owner_jid, node_name, item_id, payload_xml, publisher_jid, published_at_ms
            )
            VALUES (?, ?, ?, ?, ?, ?)
            ON CONFLICT(owner_jid, node_name, item_id) DO UPDATE SET
                seq = excluded.seq,
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
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
        let evicted_item_ids = self
            .enforce_max_items_tx(&mut tx, owner, node_name, node.config.max_items)
            .await?;
        if let Some(bookmark) = validated_bookmark.as_ref() {
            let source_version = next_notification_settings_source_version_tx(&mut tx).await?;
            let payload = item.payload.as_ref().ok_or_else(|| {
                XmppError::internal("validated bookmark publish lost payload".to_string())
            })?;
            let mutation = derive_validated_bookmark_projection_mutation(
                owner,
                bookmark,
                payload,
                ConversationKind::PrivateGroup,
                published_at_ms,
                source_version,
            )
            .map_err(|error| XmppError::internal(error.to_string()))?;
            apply_projection_mutation_tx(&mut tx, mutation).await?;
            for evicted_item_id in &evicted_item_ids {
                delete_bookmark_projection_for_item_tx(&mut tx, owner, evicted_item_id).await?;
            }
        }
        if let Some((dnd_owner, parsed)) = dnd_publish_owner {
            // published_at_ms doubles as the LWW source_version — see
            // `dnd_projection::upsert_dnd_projection_tx` for the
            // ON CONFLICT guard that keeps a slow-to-commit older
            // publish from stomping a newer one.
            let projection = DndProjection {
                owner_bare_jid: dnd_owner,
                state: parsed,
                source_version: published_at_ms,
                updated_at_ms: published_at_ms,
            };
            upsert_dnd_projection_tx(&mut tx, &projection)
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;

        Ok(PublishResult {
            item_id,
            node_created,
            evicted_item_ids,
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
        if is_bookmarks_node(node_name) {
            let mut tx = self
                .db
                .begin()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            let affected = tx
                .execute(
                    "DELETE FROM pubsub_items WHERE owner_jid = ? AND node_name = ? AND item_id = ?",
                    crate::db_params![owner.to_string(), node_name, item_id],
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            if affected > 0 {
                delete_bookmark_projection_for_item_tx(&mut tx, owner, item_id).await?;
            }
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(affected > 0);
        }
        if is_dnd_node(node_name) {
            // Retracting an `urn:waddle:dnd:0` item MUST clear the
            // server-side projection in the same tx — leaving a stale
            // projection would suppress push notifications even though
            // the user explicitly cleared their DND state.
            let mut tx = self
                .db
                .begin()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            let affected = tx
                .execute(
                    "DELETE FROM pubsub_items WHERE owner_jid = ? AND node_name = ? AND item_id = ?",
                    crate::db_params![owner.to_string(), node_name, item_id],
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            if affected > 0 {
                delete_dnd_projection_tx(&mut tx, owner)
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
            }
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(affected > 0);
        }

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
        if is_bookmarks_node(node_name) {
            let mut tx = self
                .db
                .begin()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            let affected = tx
                .execute(
                    "DELETE FROM pubsub_items WHERE owner_jid = ? AND node_name = ?",
                    crate::db_params![owner.to_string(), node_name],
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            clear_bookmark_projection_tx(&mut tx, owner).await?;
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(affected);
        }
        if is_dnd_node(node_name) {
            // Purging the DND node clears the projection alongside.
            let mut tx = self
                .db
                .begin()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            let affected = tx
                .execute(
                    "DELETE FROM pubsub_items WHERE owner_jid = ? AND node_name = ?",
                    crate::db_params![owner.to_string(), node_name],
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            delete_dnd_projection_tx(&mut tx, owner)
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(affected);
        }

        let affected = self
            .execute(
                "DELETE FROM pubsub_items WHERE owner_jid = ? AND node_name = ?",
                crate::db_params![owner.to_string(), node_name],
            )
            .await?;
        Ok(affected)
    }

    async fn enforce_max_items_tx(
        &self,
        tx: &mut crate::db::Transaction<'_>,
        owner: &BareJid,
        node_name: &str,
        max_items: u32,
    ) -> Result<Vec<String>, XmppError> {
        if max_items == 0 || max_items == u32::MAX {
            return Ok(Vec::new());
        }
        let mut rows = tx
            .query(
                r#"
                DELETE FROM pubsub_items
                WHERE owner_jid = ? AND node_name = ?
                  AND seq NOT IN (
                      SELECT seq FROM pubsub_items
                      WHERE owner_jid = ? AND node_name = ?
                      ORDER BY seq DESC
                      LIMIT ?
                  )
                RETURNING item_id
                "#,
                crate::db_params![
                    owner.to_string(),
                    node_name,
                    owner.to_string(),
                    node_name,
                    max_items,
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let mut evicted_item_ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            evicted_item_ids.push(
                row.get(0)
                    .map_err(|error| XmppError::internal(error.to_string()))?,
            );
        }
        Ok(evicted_item_ids)
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

pub(super) async fn clear_bookmark_projection_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner: &BareJid,
) -> Result<(), XmppError> {
    tx.execute(
        r#"
        DELETE FROM notification_settings_projection
        WHERE owner_bare_jid = ? AND source_node = ?
        "#,
        crate::db_params![
            owner.to_string(),
            crate::notification_settings_projection::NotificationSettingsSource::Xep0402Bookmarks
                .node(),
        ],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

async fn delete_bookmark_projection_for_item_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner: &BareJid,
    item_id: &str,
) -> Result<(), XmppError> {
    let conversation_jid: BareJid = item_id
        .parse()
        .map_err(|error| XmppError::internal(format!("invalid bookmark item id: {error}")))?;
    tx.execute(
        r#"
        DELETE FROM notification_settings_projection
        WHERE owner_bare_jid = ? AND conversation_jid = ?
        "#,
        crate::db_params![owner.to_string(), conversation_jid.to_string()],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

async fn apply_projection_mutation_tx(
    tx: &mut crate::db::Transaction<'_>,
    mutation: NotificationSettingsProjectionMutation,
) -> Result<(), XmppError> {
    match mutation {
        NotificationSettingsProjectionMutation::Upsert(projection) => {
            tx.execute(
                r#"
                INSERT INTO notification_settings_projection (
                    owner_bare_jid,
                    conversation_jid,
                    conversation_kind,
                    mode,
                    source_version,
                    updated_at_ms,
                    source_node,
                    source_item_id
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(owner_bare_jid, conversation_jid) DO UPDATE SET
                    conversation_kind = excluded.conversation_kind,
                    mode = excluded.mode,
                    source_version = excluded.source_version,
                    updated_at_ms = excluded.updated_at_ms,
                    source_node = excluded.source_node,
                    source_item_id = excluded.source_item_id
                "#,
                crate::db_params![
                    projection.owner_bare_jid.to_string(),
                    projection.conversation_jid.to_string(),
                    projection.conversation_kind.as_db_value(),
                    projection.mode.element_name(),
                    projection.source_version,
                    projection.updated_at_ms,
                    projection.source.node(),
                    projection.source_item_jid.to_string(),
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        }
        NotificationSettingsProjectionMutation::Delete {
            owner_bare_jid,
            conversation_jid,
        } => {
            tx.execute(
                r#"
                DELETE FROM notification_settings_projection
                WHERE owner_bare_jid = ? AND conversation_jid = ?
                "#,
                crate::db_params![owner_bare_jid.to_string(), conversation_jid.to_string()],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        }
    }
    Ok(())
}

async fn next_notification_settings_source_version_tx(
    tx: &mut crate::db::Transaction<'_>,
) -> Result<i64, XmppError> {
    tx.execute(
        r#"
        INSERT INTO notification_settings_projection_source_version (id, current_version)
        VALUES (1, 0)
        ON CONFLICT(id) DO NOTHING
        "#,
        (),
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;

    let mut rows = tx
        .query(
            r#"
            UPDATE notification_settings_projection_source_version
            SET current_version = current_version + 1
            WHERE id = 1
            RETURNING current_version
            "#,
            (),
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
        .ok_or_else(|| XmppError::internal("missing projection source version".to_string()))?;
    row.get(0)
        .map_err(|error| XmppError::internal(error.to_string()))
}

fn is_bookmarks_node(node_name: &str) -> bool {
    node_name == waddle_xmpp::xep::xep0402::PEP_NODE
}

fn is_dnd_node(node_name: &str) -> bool {
    node_name == waddle_xmpp::xep::xep_waddle_dnd::PEP_NODE_WADDLE_DND
}
