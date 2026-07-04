//! First-party XEP-0357 enable/disable: transactional coupling between
//! the Push Service node/device state and the user-server
//! `push_registrations` store.

use jid::BareJid;
use minidom::Element;
use waddle_xmpp::push::{PushError, PushSubscription};
use waddle_xmpp::XmppError;

use super::devices::{count_active_devices_for_node_tx, validate_len};
use super::nodes::{get_node_tx, MAX_NODE_ID_LEN};
use super::publish_jobs::delete_retryable_publish_jobs_for_node_tx;
use super::store::{lock_node_tx, lock_owner_tx, DatabasePushServiceStore};
use super::types::PushNodeStatus;

fn push_error_to_xmpp_error(error: PushError) -> XmppError {
    match error {
        PushError::StorageError(message)
            if message.contains("provider credential fields are not allowed") =>
        {
            XmppError::bad_request(Some(message))
        }
        other => XmppError::internal(other.to_string()),
    }
}

pub(super) async fn ensure_active_registration_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    push_service_jid: &str,
    node: &str,
) -> Result<(), XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT 1
            FROM push_registrations
            WHERE owner_bare_jid = ?
              AND push_service_jid = ?
              AND node = ?
              AND status = ?
            LIMIT 1
            "#,
            crate::db_params![
                owner_bare_jid.to_string(),
                push_service_jid,
                node,
                crate::push_registrations::STATUS_ENABLED,
            ],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    if rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
        .is_some()
    {
        return Ok(());
    }
    Err(XmppError::item_not_found(Some(
        "XEP-0357 registration not active".to_string(),
    )))
}

async fn validate_first_party_enable_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    node: &str,
    now_ms: i64,
) -> Result<(), XmppError> {
    if get_node_tx(tx, node).await?.is_none() {
        return Err(XmppError::item_not_found(Some(
            "Push node not found".to_string(),
        )));
    }
    lock_node_tx(tx, node, now_ms).await?;
    let push_node = get_node_tx(tx, node)
        .await?
        .ok_or_else(|| XmppError::internal("Push Service node was not persisted"))?;
    if push_node.owner_bare_jid != *owner_bare_jid {
        return Err(XmppError::forbidden(Some(
            "Push node belongs to another user".to_string(),
        )));
    }
    if push_node.status != PushNodeStatus::Active {
        return Err(XmppError::item_not_found(Some(
            "Push node not active".to_string(),
        )));
    }
    if count_active_devices_for_node_tx(tx, node).await? == 0 {
        return Err(XmppError::bad_request(Some(
            "Push node has no active registered devices".to_string(),
        )));
    }
    Ok(())
}

impl DatabasePushServiceStore {
    pub async fn validate_first_party_enable_node(
        &self,
        owner_bare_jid: &BareJid,
        node: &str,
    ) -> Result<(), XmppError> {
        validate_len("Push Service node", node, MAX_NODE_ID_LEN)?;
        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        validate_first_party_enable_node_tx(&mut tx, owner_bare_jid, node, now_ms).await?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(())
    }

    pub async fn register_first_party_node_for_owner(
        &self,
        owner_bare_jid: &BareJid,
        service_jid: &str,
        node: &str,
        publish_options: Option<&Element>,
    ) -> Result<(), XmppError> {
        validate_len("Push Service node", node, MAX_NODE_ID_LEN)?;
        self.ensure_xep0060_push_node_for_owner(owner_bare_jid, node)
            .await?;
        let now_ms = crate::time::now_ms();
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        lock_owner_tx(&mut tx, owner_bare_jid, now_ms).await?;
        validate_first_party_enable_node_tx(&mut tx, owner_bare_jid, node, now_ms).await?;
        crate::push_registrations::register_subscription_tx(
            &mut tx,
            &PushSubscription {
                user_jid: owner_bare_jid.to_string(),
                service_jid: service_jid.to_string(),
                node: Some(node.to_string()),
                publish_options: publish_options.cloned(),
                endpoint: None,
                p256dh: None,
                auth_key: None,
            },
        )
        .await
        .map_err(push_error_to_xmpp_error)?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(())
    }

    pub async fn remove_registered_nodes_for_owner(
        &self,
        owner_bare_jid: &BareJid,
        service_jid: &str,
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

        let registered_nodes = crate::push_registrations::registered_nodes_for_disable_tx(
            &mut tx,
            owner_bare_jid,
            service_jid,
            node,
        )
        .await
        .map_err(push_error_to_xmpp_error)?;
        if registered_nodes.is_empty() {
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(0);
        }

        let mut registered_push_nodes = registered_nodes
            .iter()
            .filter(|node| !node.is_empty())
            .cloned()
            .collect::<Vec<_>>();
        registered_push_nodes.sort();
        registered_push_nodes.dedup();
        for node in &registered_push_nodes {
            lock_node_tx(&mut tx, node, now_ms).await?;
            delete_retryable_publish_jobs_for_node_tx(&mut tx, owner_bare_jid, node).await?;
        }
        let removed = crate::push_registrations::remove_subscription_tx(
            &mut tx,
            owner_bare_jid,
            service_jid,
            node,
        )
        .await
        .map_err(push_error_to_xmpp_error)?;

        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(removed)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use waddle_xmpp::push::PushSubscriptionStore;

    use crate::push_service::test_support::{
        notification_item, owner, publish_options_with_field, store,
    };
    use crate::push_service::{PushDevicePlatform, PushDeviceRegistration};

    #[tokio::test]
    async fn first_party_disable_rolls_back_when_registration_delete_fails() {
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
        let registration_store =
            crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
                .await
                .expect("registration store");
        registration_store
            .register(PushSubscription {
                user_jid: owner.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some(node.node().to_string()),
                publish_options: Some(publish_options_with_field("secret", "server-secret")),
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("registration");
        store
            .execute(
                r#"
                CREATE TRIGGER fail_push_registration_delete
                BEFORE DELETE ON push_registrations
                BEGIN
                    SELECT RAISE(ABORT, 'forced push registration delete failure');
                END
                "#,
                (),
            )
            .await
            .expect("failure trigger");

        store
            .remove_registered_nodes_for_owner(&owner, "push.example.com", Some(node.node()))
            .await
            .expect_err("forced delete failure should abort first-party disable transaction");

        let active_node = store
            .get_node_for_owner(&owner, node.node())
            .await
            .expect("active node lookup")
            .expect("node should remain active after rollback");
        let active_device = store
            .get_device_for_owner_on_node(&owner, node.node(), "web-1")
            .await
            .expect("device lookup")
            .expect("device should remain after rollback");
        let registrations = registration_store
            .get_for_user(&owner.to_string())
            .await
            .expect("registrations");
        let publish = store
            .publish_notification_from_user_server(
                node.node(),
                &notification_item("after-disable-rollback"),
                &owner,
            )
            .await
            .expect("push node remains usable after rollback");

        assert_eq!(active_node.status, PushNodeStatus::Active);
        assert_eq!(
            active_device.provider_endpoint(),
            Some("https://push.example.com/endpoint")
        );
        assert_eq!(active_device.provider_token(), Some("provider-secret"));
        assert_eq!(active_device.provider_key_material(), Some("provider-key"));
        assert_eq!(registrations.len(), 1);
        assert_eq!(registrations[0].node.as_deref(), Some(node.node()));
        assert_eq!(publish.attempted_devices(), 1);
    }

    #[tokio::test]
    async fn first_party_disable_preserves_device_state_and_retires_queued_jobs() {
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
        let registration_store =
            crate::push_registrations::DatabasePushRegistrationStore::new(store.database())
                .await
                .expect("registration store");
        registration_store
            .register(PushSubscription {
                user_jid: owner.to_string(),
                service_jid: "push.example.com".to_string(),
                node: Some(node.node().to_string()),
                publish_options: Some(publish_options_with_field("secret", "server-secret")),
                endpoint: None,
                p256dh: None,
                auth_key: None,
            })
            .await
            .expect("registration");
        store
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("stale-before-disable"),
                &owner,
            )
            .await
            .expect("enqueue stale job");

        let removed = store
            .remove_registered_nodes_for_owner(&owner, "push.example.com", Some(node.node()))
            .await
            .expect("remove first-party registration");
        let active_node = store
            .get_node_for_owner(&owner, node.node())
            .await
            .expect("node lookup")
            .expect("node remains active");
        let active_device = store
            .get_device_for_owner_on_node(&owner, node.node(), "web-1")
            .await
            .expect("device lookup")
            .expect("device remains provisioned");
        let registrations = registration_store
            .get_for_user(&owner.to_string())
            .await
            .expect("registrations");
        let reactivated = store.ensure_node(&owner, "web").await.expect("same node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device refresh");
        let drained = store
            .drain_queued_notification_publish_jobs(16)
            .await
            .expect("drain after disable");

        assert_eq!(removed, 1);
        assert_eq!(active_node.status, PushNodeStatus::Active);
        assert_eq!(reactivated.node(), node.node());
        assert_eq!(
            active_device.provider_endpoint(),
            Some("https://push.example.com/endpoint")
        );
        assert_eq!(active_device.provider_token(), Some("provider-secret"));
        assert_eq!(active_device.provider_key_material(), Some("provider-key"));
        assert!(registrations.is_empty());
        assert!(store
            .queued_publish_jobs()
            .await
            .expect("queued")
            .is_empty());
        assert!(drained.is_empty());
    }
}
