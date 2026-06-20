use jid::BareJid;
use tracing::warn;
use waddle_xmpp::pubsub::{PubSubItem, PubSubNode, PublishResult, StoredItem};
use waddle_xmpp::xep::xep_waddle_dm_bookmarks::{is_dm_bookmarks_node, DmBookmark};
use waddle_xmpp::XmppError;

use super::DatabasePubSubStorage;
use crate::dnd_projection::{delete_dnd_projection_tx, upsert_dnd_projection_tx, DndProjection};
use crate::notification_settings_projection::{
    derive_dm_bookmark_projection_mutation, derive_validated_bookmark_projection_mutation,
    validate_dm_bookmark_publish, validate_xep0402_bookmark_publish, ConversationKind,
    NotificationSettingsProjectionMutation, NotificationSettingsSource,
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
        self.publish_item_internal(owner, node_name, item, publisher, auto_create, false)
            .await
    }

    pub(super) async fn publish_item_if_missing_or_publisher_impl(
        &self,
        owner: &BareJid,
        node_name: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        auto_create: bool,
    ) -> Result<PublishResult, XmppError> {
        self.publish_item_internal(owner, node_name, item, Some(publisher), auto_create, true)
            .await
    }

    async fn publish_item_internal(
        &self,
        owner: &BareJid,
        node_name: &str,
        item: &PubSubItem,
        publisher: Option<&BareJid>,
        auto_create: bool,
        require_same_publisher: bool,
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
        // DM-bookmark carrier (`urn:waddle:dm-bookmarks:0`) — the direct
        // (1:1) counterpart of the XEP-0402 MUC bookmark above. Validate
        // up-front so a malformed `<dm-bookmark>` gets a `<bad-request/>`
        // instead of leaving the PEP item in place with a stale Direct
        // projection alongside.
        //
        // Restrict publishes to the case where the publisher IS the owner
        // (mirroring the `urn:waddle:dnd:0` branch below). DM notification
        // settings are user-identity state: even if the owner grants a peer
        // an explicit Publisher affiliation or sets `publish_model=open` on
        // the node, the peer MUST NOT be able to write the owner's per-
        // contact overrides (muting them, or flipping the #719 rich-payload
        // opt-in so push notifications leak message bodies). Allowing the
        // wire-level publish while skipping the projection write would leave
        // `pubsub_items` and the projection in disagreement, so reject
        // outright with `<forbidden/>`.
        let validated_dm_bookmark: Option<DmBookmark> = if is_dm_bookmarks_node(node_name) {
            match publisher {
                Some(publisher_jid) if publisher_jid == owner => {}
                _ => {
                    warn!(
                        owner = %owner,
                        publisher = ?publisher,
                        "urn:waddle:dm-bookmarks:0 publish rejected: publisher is not the node owner"
                    );
                    return Err(XmppError::forbidden(Some(
                        "urn:waddle:dm-bookmarks:0 may only be published by the node owner"
                            .to_string(),
                    )));
                }
            }
            // The DM carrier requires the PubSub item id to be the
            // contact's bare JID. Reject a missing id explicitly here —
            // otherwise the `item_id` UUID fallback above flows into
            // `validate_dm_bookmark_publish` and surfaces a confusing
            // "invalid JID: <uuid>" instead of a carrier-specific
            // `<bad-request/>` (Copilot review).
            if item.id.is_none() {
                return Err(XmppError::bad_request(Some(
                    "urn:waddle:dm-bookmarks:0 publish requires the item id to be the contact bare JID"
                        .to_string(),
                )));
            }
            let payload = item.payload.as_ref().ok_or_else(|| {
                XmppError::bad_request(Some(
                    "Waddle DM-bookmark publish requires a <dm-bookmark> payload".to_string(),
                ))
            })?;
            Some(
                validate_dm_bookmark_publish(&item_id, payload)
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
                    return Err(XmppError::pubsub_invalid_payload(Some(format!(
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
                XmppError::pubsub_payload_required(Some(
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
                    XmppError::pubsub_invalid_payload(Some(error.to_string()))
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

        let rows_affected = if require_same_publisher {
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
                WHERE pubsub_items.publisher_jid = excluded.publisher_jid
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
            .map_err(|error| XmppError::internal(error.to_string()))?
        } else {
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
            .map_err(|error| XmppError::internal(error.to_string()))?
        };
        if rows_affected == 0 {
            return Err(XmppError::forbidden(Some(
                "PubSub item belongs to a different publisher".to_string(),
            )));
        }
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
                delete_bookmark_projection_for_item_tx(
                    &mut tx,
                    owner,
                    evicted_item_id,
                    NotificationSettingsSource::Xep0402Bookmarks,
                )
                .await?;
            }
        }
        if let Some(dm_bookmark) = validated_dm_bookmark.as_ref() {
            let source_version = next_notification_settings_source_version_tx(&mut tx).await?;
            let mutation = derive_dm_bookmark_projection_mutation(
                owner,
                dm_bookmark,
                published_at_ms,
                source_version,
            )
            .map_err(|error| XmppError::internal(error.to_string()))?;
            apply_projection_mutation_tx(&mut tx, mutation).await?;
            // An eviction can drop a DM item whose projection row keys on
            // its item id (the contact bare JID); clear it in the same tx.
            // Scope the delete to the DM source so a DM eviction never
            // clobbers a MUC-sourced row that happens to share the same
            // conversation JID (the projection PK is source-agnostic).
            for evicted_item_id in &evicted_item_ids {
                delete_bookmark_projection_for_item_tx(
                    &mut tx,
                    owner,
                    evicted_item_id,
                    NotificationSettingsSource::WaddleDmBookmarks,
                )
                .await?;
            }
        }
        if let Some((dnd_owner, parsed)) = dnd_publish_owner {
            // Monotonic DB-backed source_version. The earlier
            // implementation used `published_at_ms` (wall-clock); an
            // NTP backwards-jump would let a newer publish look older
            // and silently drop from the projection via the LWW guard
            // while `pubsub_items` still committed. Switching to a
            // singleton counter (same pattern as the bookmarks
            // projection) closes that hole — see
            // `dnd_projection::upsert_dnd_projection_tx` for the
            // ON CONFLICT guard the counter is paired with.
            let source_version = next_dnd_projection_source_version_tx(&mut tx).await?;
            let projection = DndProjection {
                owner_bare_jid: dnd_owner,
                state: parsed,
                source_version,
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
        if is_bookmarks_node(node_name) || is_dm_bookmarks_node(node_name) {
            // Both the XEP-0402 MUC carrier and the Waddle DM carrier key
            // their projection row on (owner, conversation_jid=item_id).
            // The projection PK is source-agnostic and the two carriers
            // can target the SAME conversation JID, so scope the delete to
            // the carrier whose item is actually being retracted —
            // otherwise a DM retract could clobber a MUC-sourced row (or
            // vice-versa).
            let source = if is_dm_bookmarks_node(node_name) {
                NotificationSettingsSource::WaddleDmBookmarks
            } else {
                NotificationSettingsSource::Xep0402Bookmarks
            };
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
                delete_bookmark_projection_for_item_tx(&mut tx, owner, item_id, source).await?;
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
        if is_bookmarks_node(node_name) || is_dm_bookmarks_node(node_name) {
            // Clear exactly the projection rows fed by the carrier being
            // purged: the XEP-0402 MUC node clears `Xep0402Bookmarks`
            // rows, the Waddle DM node clears `WaddleDmBookmarks` rows.
            // Keying on the source (not the conversation JID) leaves the
            // other carrier's rows untouched.
            let source = if is_dm_bookmarks_node(node_name) {
                NotificationSettingsSource::WaddleDmBookmarks
            } else {
                NotificationSettingsSource::Xep0402Bookmarks
            };
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
            clear_projection_for_source_tx(&mut tx, owner, source).await?;
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

/// Clear every projection row fed by the XEP-0402 MUC bookmark carrier
/// for `owner`. Used by node-delete (`node.rs`) when the deleted node is
/// the XEP-0402 bookmarks node.
pub(super) async fn clear_bookmark_projection_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner: &BareJid,
) -> Result<(), XmppError> {
    clear_projection_for_source_tx(tx, owner, NotificationSettingsSource::Xep0402Bookmarks).await
}

/// Clear every projection row fed by the Waddle DM-bookmark carrier
/// (`urn:waddle:dm-bookmarks:0`) for `owner`. Used by node-delete
/// (`node.rs`) when the deleted node is the DM-bookmarks node —
/// otherwise deleting the node would orphan every `WaddleDmBookmarks`
/// projection row (stale push suppression with no wire way to clear it).
pub(super) async fn clear_dm_bookmark_projection_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner: &BareJid,
) -> Result<(), XmppError> {
    clear_projection_for_source_tx(tx, owner, NotificationSettingsSource::WaddleDmBookmarks).await
}

/// Clear every projection row fed by `source` for `owner`, keying on the
/// `source_node` column. Used by the carrier-purge paths so purging one
/// carrier (MUC vs DM) leaves the other carrier's rows untouched.
async fn clear_projection_for_source_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner: &BareJid,
    source: NotificationSettingsSource,
) -> Result<(), XmppError> {
    tx.execute(
        r#"
        DELETE FROM notification_settings_projection
        WHERE owner_bare_jid = ? AND source_node = ?
        "#,
        crate::db_params![owner.to_string(), source.node()],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

/// Delete the projection row keyed on `(owner, conversation_jid=item_id)`
/// — but ONLY when it was fed by `source`.
///
/// The projection PK `(owner_bare_jid, conversation_jid)` is
/// source-agnostic, and the XEP-0402 validator accepts any bare JID
/// with a localpart as a conference bookmark id, so a MUC bookmark id
/// can equal a DM peer JID and the two carriers can target the SAME
/// projection row. Scoping the DELETE by `source_node` keeps a DM
/// retract/eviction from clobbering a MUC-sourced row (and vice-versa)
/// when both carriers point at the same conversation JID. Callers pass
/// the source of the carrier whose item is being removed.
async fn delete_bookmark_projection_for_item_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner: &BareJid,
    item_id: &str,
    source: NotificationSettingsSource,
) -> Result<(), XmppError> {
    let conversation_jid: BareJid = item_id
        .parse()
        .map_err(|error| XmppError::internal(format!("invalid bookmark item id: {error}")))?;
    tx.execute(
        r#"
        DELETE FROM notification_settings_projection
        WHERE owner_bare_jid = ? AND conversation_jid = ? AND source_node = ?
        "#,
        crate::db_params![
            owner.to_string(),
            conversation_jid.to_string(),
            source.node()
        ],
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
                    rich_payload_opt_in,
                    source_version,
                    updated_at_ms,
                    source_node,
                    source_item_id
                )
                VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                ON CONFLICT(owner_bare_jid, conversation_jid) DO UPDATE SET
                    conversation_kind = excluded.conversation_kind,
                    mode = excluded.mode,
                    rich_payload_opt_in = excluded.rich_payload_opt_in,
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
                    i64::from(projection.rich_payload_opt_in),
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
            source,
        } => {
            // Scope the delete to the deriving carrier's `source_node`.
            // An empty/absent `<notify>` on one carrier MUST NOT clear a
            // row the OTHER carrier wrote for the same `conversation_jid`
            // (the DM vs XEP-0402 same-JID overlap the retract/eviction
            // paths also guard).
            tx.execute(
                r#"
                DELETE FROM notification_settings_projection
                WHERE owner_bare_jid = ? AND conversation_jid = ? AND source_node = ?
                "#,
                crate::db_params![
                    owner_bare_jid.to_string(),
                    conversation_jid.to_string(),
                    source.node(),
                ],
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
    next_singleton_counter_tx(tx, "notification_settings_projection_source_version").await
}

async fn next_dnd_projection_source_version_tx(
    tx: &mut crate::db::Transaction<'_>,
) -> Result<i64, XmppError> {
    next_singleton_counter_tx(tx, "dnd_projection_source_version").await
}

/// Atomic monotonic-counter bump inside an open transaction. The
/// singleton row is keyed on `id = 1` (check constraint at the DDL
/// site) so two concurrent transactions serialize on the row's write
/// lock instead of racing. Returns the post-increment value.
///
/// `&'static str` enforces at the type system level that callers
/// cannot pass user input (no SQL-injection surface despite the
/// `format!`); each call site is a literal table name pinned by
/// the schema DDL.
async fn next_singleton_counter_tx(
    tx: &mut crate::db::Transaction<'_>,
    table: &'static str,
) -> Result<i64, XmppError> {
    tx.execute(
        &format!(
            r#"
            INSERT INTO {table} (id, current_version)
            VALUES (1, 0)
            ON CONFLICT(id) DO NOTHING
            "#
        ),
        (),
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;

    let mut rows = tx
        .query(
            &format!(
                r#"
                UPDATE {table}
                SET current_version = current_version + 1
                WHERE id = 1
                RETURNING current_version
                "#
            ),
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
