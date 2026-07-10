//! Push node lifecycle: one node per `(owner, app-id)`, quotas,
//! disable/reactivate, and retained-disabled-node pruning.

use jid::BareJid;
use waddle_xmpp::XmppError;

use super::devices::validate_len;
use super::store::{lock_node_tx, lock_owner_tx, DatabasePushServiceStore};
use super::types::{
    PushNodeStatus, PushServiceNode, DEVICE_STATUS_DISABLED, NODE_STATUS_ACTIVE,
    NODE_STATUS_DISABLED,
};

pub(super) const MAX_PUSH_NODES_PER_OWNER: i64 = 16;

pub(super) const MAX_RETAINED_DISABLED_NODES_PER_OWNER: i64 = 64;

pub(super) const MAX_APP_ID_LEN: usize = 128;

pub(super) const MAX_NODE_ID_LEN: usize = 256;

pub(super) async fn get_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
) -> Result<Option<PushServiceNode>, XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT node, owner_bare_jid, app_id, status, created_at_ms, updated_at_ms
            FROM push_nodes
            WHERE node = ?
            "#,
            crate::db_params![node],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    Ok(Some(decode_node(&row)?))
}

async fn get_node_for_owner_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    node: &str,
) -> Result<Option<PushServiceNode>, XmppError> {
    let Some(push_node) = get_node_tx(tx, node).await? else {
        return Ok(None);
    };
    if push_node.owner_bare_jid == *owner_bare_jid {
        Ok(Some(push_node))
    } else {
        Ok(None)
    }
}

async fn find_node_by_owner_app_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    app_id: &str,
) -> Result<Option<PushServiceNode>, XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT node, owner_bare_jid, app_id, status, created_at_ms, updated_at_ms
            FROM push_nodes
            WHERE owner_bare_jid = ? AND app_id = ?
            "#,
            crate::db_params![owner_bare_jid.to_string(), app_id],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    Ok(Some(decode_node(&row)?))
}

async fn count_active_nodes_for_owner_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
) -> Result<i64, XmppError> {
    let mut rows = tx
        .query(
            "SELECT COUNT(*) FROM push_nodes WHERE owner_bare_jid = ? AND status = ?",
            crate::db_params![owner_bare_jid.to_string(), NODE_STATUS_ACTIVE],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
        .ok_or_else(|| XmppError::internal("Push node count query returned no row"))?;
    row.get(0)
        .map_err(|error| XmppError::internal(error.to_string()))
}

async fn node_names_for_owner_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
) -> Result<Vec<String>, XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT node
            FROM push_nodes
            WHERE owner_bare_jid = ?
            ORDER BY node ASC
            "#,
            crate::db_params![owner_bare_jid.to_string()],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
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

async fn prune_disabled_nodes_for_owner_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    retain: i64,
) -> Result<(), XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT node
            FROM push_nodes
            WHERE owner_bare_jid = ? AND status = ?
            ORDER BY updated_at_ms DESC, node DESC
            "#,
            crate::db_params![owner_bare_jid.to_string(), NODE_STATUS_DISABLED],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let mut seen = 0_i64;
    let mut stale_nodes = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    {
        seen += 1;
        if seen > retain {
            stale_nodes.push(
                row.get::<String>(0)
                    .map_err(|error| XmppError::internal(error.to_string()))?,
            );
        }
    }
    for node in stale_nodes {
        tx.execute(
            "DELETE FROM push_nodes WHERE owner_bare_jid = ? AND node = ? AND status = ?",
            crate::db_params![owner_bare_jid.to_string(), node, NODE_STATUS_DISABLED],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    }
    Ok(())
}

fn validate_app_id(app_id: &str) -> Result<(), XmppError> {
    if app_id.is_empty() {
        return Err(XmppError::bad_request(Some(
            "Push Service app-id is required".to_string(),
        )));
    }
    validate_len("Push Service app-id", app_id, MAX_APP_ID_LEN)
}

pub(super) async fn ensure_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    app_id: &str,
    now_ms: i64,
) -> Result<PushServiceNode, XmppError> {
    validate_app_id(app_id)?;
    lock_owner_tx(tx, owner_bare_jid, now_ms).await?;
    if let Some(node) = find_node_by_owner_app_tx(tx, owner_bare_jid, app_id).await? {
        if node.status == PushNodeStatus::Active {
            return Ok(node);
        }
        if count_active_nodes_for_owner_tx(tx, owner_bare_jid).await? >= MAX_PUSH_NODES_PER_OWNER {
            return Err(XmppError::bad_request(Some(format!(
                "Push Service active node quota exceeded; max {MAX_PUSH_NODES_PER_OWNER} active nodes per owner"
            ))));
        }
        tx.execute(
            r#"
            UPDATE push_nodes
            SET status = ?, updated_at_ms = ?
            WHERE node = ?
            "#,
            crate::db_params![PushNodeStatus::Active.as_str(), now_ms, node.node()],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
        return get_node_tx(tx, node.node())
            .await?
            .ok_or_else(|| XmppError::internal("Push Service node was not persisted"));
    }
    prune_disabled_nodes_for_owner_tx(tx, owner_bare_jid, MAX_RETAINED_DISABLED_NODES_PER_OWNER)
        .await?;
    if count_active_nodes_for_owner_tx(tx, owner_bare_jid).await? >= MAX_PUSH_NODES_PER_OWNER {
        return Err(XmppError::bad_request(Some(format!(
            "Push Service active node quota exceeded; max {MAX_PUSH_NODES_PER_OWNER} active nodes per owner"
        ))));
    }

    let node_name = format!("urn:waddle:push-node:{}", uuid::Uuid::new_v4());
    tx.execute(
        r#"
        INSERT INTO push_nodes (
            node,
            owner_bare_jid,
            app_id,
            status,
            created_at_ms,
            updated_at_ms
        ) VALUES (?, ?, ?, ?, ?, ?)
        ON CONFLICT(owner_bare_jid, app_id) DO NOTHING
        "#,
        crate::db_params![
            node_name,
            owner_bare_jid.to_string(),
            app_id,
            PushNodeStatus::Active.as_str(),
            now_ms,
            now_ms,
        ],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;

    find_node_by_owner_app_tx(tx, owner_bare_jid, app_id)
        .await?
        .ok_or_else(|| XmppError::internal("Push Service node was not persisted"))
}

fn decode_node(row: &crate::db::Row) -> Result<PushServiceNode, XmppError> {
    let owner_bare_jid: String = row
        .get(1)
        .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(PushServiceNode {
        node: row
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        owner_bare_jid: owner_bare_jid.parse().map_err(|error| {
            XmppError::internal(format!("Invalid stored push owner JID: {error}"))
        })?,
        app_id: row
            .get(2)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        status: PushNodeStatus::parse(
            &row.get::<String>(3)
                .map_err(|error| XmppError::internal(error.to_string()))?,
        )?,
        created_at_ms: row
            .get(4)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        updated_at_ms: row
            .get(5)
            .map_err(|error| XmppError::internal(error.to_string()))?,
    })
}

impl DatabasePushServiceStore {
    pub async fn ensure_node(
        &self,
        owner_bare_jid: &BareJid,
        app_id: &str,
    ) -> Result<PushServiceNode, XmppError> {
        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let node = ensure_node_tx(&mut tx, owner_bare_jid, app_id, now_ms).await?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        self.ensure_xep0060_push_node_for_owner(owner_bare_jid, node.node())
            .await?;
        Ok(node)
    }

    pub async fn get_node(&self, node: &str) -> Result<Option<PushServiceNode>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT node, owner_bare_jid, app_id, status, created_at_ms, updated_at_ms
                FROM push_nodes
                WHERE node = ?
                "#,
                crate::db_params![node],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_node(&row)?))
    }

    pub async fn get_node_for_owner(
        &self,
        owner_bare_jid: &BareJid,
        node: &str,
    ) -> Result<Option<PushServiceNode>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT node, owner_bare_jid, app_id, status, created_at_ms, updated_at_ms
                FROM push_nodes
                WHERE owner_bare_jid = ? AND node = ? AND status = ?
                "#,
                crate::db_params![owner_bare_jid.to_string(), node, NODE_STATUS_ACTIVE],
            )
            .await?;
        let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        else {
            return Ok(None);
        };
        Ok(Some(decode_node(&row)?))
    }

    pub async fn list_node_names_for_owner(
        &self,
        owner_bare_jid: &BareJid,
    ) -> Result<Vec<String>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT node
                FROM push_nodes
                WHERE owner_bare_jid = ? AND status = ?
                ORDER BY node ASC
                "#,
                crate::db_params![owner_bare_jid.to_string(), NODE_STATUS_ACTIVE],
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

    pub async fn disable_nodes_for_owner(
        &self,
        owner_bare_jid: &BareJid,
        node: Option<&str>,
    ) -> Result<u64, XmppError> {
        if let Some(node) = node {
            validate_len("Push Service node", node, MAX_NODE_ID_LEN)?;
        }
        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        lock_owner_tx(&mut tx, owner_bare_jid, now_ms).await?;

        let nodes = match node {
            Some(node) => {
                if get_node_for_owner_tx(&mut tx, owner_bare_jid, node)
                    .await?
                    .is_some()
                {
                    vec![node.to_string()]
                } else {
                    tx.commit()
                        .await
                        .map_err(|error| XmppError::internal(error.to_string()))?;
                    return Ok(0);
                }
            }
            None => node_names_for_owner_tx(&mut tx, owner_bare_jid).await?,
        };

        let mut affected_devices = 0;
        for node in &nodes {
            lock_node_tx(&mut tx, node, now_ms).await?;
            affected_devices += tx
                .execute(
                    r#"
                    UPDATE push_devices
                    SET status = ?,
                        provider_endpoint = NULL,
                        provider_token = NULL,
                        provider_key_material = NULL,
                        last_error = ?,
                        updated_at_ms = ?
                    WHERE node = ?
                    "#,
                    crate::db_params![
                        DEVICE_STATUS_DISABLED,
                        "disabled via Push Service admin",
                        now_ms,
                        node,
                    ],
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            tx.execute(
                r#"
                UPDATE push_nodes
                SET status = ?, updated_at_ms = ?
                WHERE node = ?
                "#,
                crate::db_params![PushNodeStatus::Disabled.as_str(), now_ms, node],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        }

        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(affected_devices)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::push_service::test_support::{
        assert_item_not_found, notification_item, owner, store,
    };
    use crate::push_service::{PushDevicePlatform, PushDeviceRegistration};

    #[tokio::test]
    async fn node_quota_counts_active_nodes_not_retired_nodes() {
        let store = store().await;
        let owner = owner();
        for idx in 0..MAX_PUSH_NODES_PER_OWNER {
            let node = store
                .ensure_node(&owner, &format!("app-{idx}"))
                .await
                .expect("node within quota");
            store
                .disable_nodes_for_owner(&owner, Some(node.node()))
                .await
                .expect("disable retained node");
        }

        let fresh = store
            .ensure_node(&owner, "app-after-retired-quota")
            .await
            .expect("retired disabled nodes must not permanently exhaust active quota");

        assert_eq!(fresh.status, PushNodeStatus::Active);
    }

    #[tokio::test]
    async fn disable_nodes_for_owner_disables_node_and_clears_provider_credentials() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test")
                    .with_provider_endpoint(Some("https://push.example.com/endpoint".to_string()))
                    .with_provider_token(Some("provider-secret".to_string()))
                    .with_provider_key_material(Some("provider-key".to_string())),
            )
            .await
            .expect("device");

        assert_eq!(
            store
                .disable_nodes_for_owner(&owner, Some(node.node()))
                .await
                .expect("disable node"),
            1
        );

        let disabled_node = store
            .get_node(node.node())
            .await
            .expect("node lookup")
            .expect("node");
        let disabled_device = store
            .get_device_for_owner_on_node(&owner, node.node(), "web-1")
            .await
            .expect("device lookup")
            .expect("device");
        let publish_err = store
            .publish_notification_from_user_server(
                node.node(),
                &notification_item("disabled-node"),
                &owner,
            )
            .await
            .expect_err("disabled node rejects publish");

        assert_eq!(disabled_node.status, PushNodeStatus::Disabled);
        assert_eq!(disabled_device.provider_endpoint(), None);
        assert_eq!(disabled_device.provider_token(), None);
        assert_eq!(disabled_device.provider_key_material(), None);
        assert!(matches!(
            publish_err,
            XmppError::Stanza {
                condition: waddle_xmpp::StanzaErrorCondition::ItemNotFound,
                ..
            }
        ));
        assert_item_not_found(
            store
                .upsert_device(
                    &owner,
                    PushDeviceRegistration::new(
                        "web-2",
                        node.node(),
                        PushDevicePlatform::Web,
                        "test",
                    )
                    .with_provider_token(Some("stale-secret".to_string())),
                )
                .await
                .expect_err("disabled node rejects stale device registration"),
        );

        let reenabled_node = store
            .ensure_node(&owner, "web")
            .await
            .expect("reenable node");
        let publish_result = store
            .publish_notification_from_user_server(
                reenabled_node.node(),
                &notification_item("reenabled-node"),
                &owner,
            )
            .await
            .expect("reenabled publish");

        assert_eq!(reenabled_node.node(), node.node());
        assert_eq!(publish_result.attempted_devices(), 0);
    }
}
