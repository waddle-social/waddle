//! XEP-0060 PubSub backing for the Push Service: node provisioning on
//! the configured PubSub boundary, durable publish persistence, and
//! XEP-0357 `<notification/>` payload validation.

use std::sync::Arc;

use jid::BareJid;
use minidom::Element;
use waddle_xmpp::pubsub::{Affiliation, NodeConfig, PubSubItem, PubSubStorage};
use waddle_xmpp::xep::xep0357::NS_PUSH;
use waddle_xmpp::XmppError;

use super::store::DatabasePushServiceStore;
use super::types::{PushNodeStatus, PushPublishJob};

pub(super) fn validate_xep0357_notification(item: &PubSubItem) -> Result<(), XmppError> {
    // XEP-0060 §7.1.3 publish errors: surface the typed PubSub
    // extension conditions instead of bare `<bad-request/>` so an
    // external user-server gets the wire-required hint
    // (`<payload-required/>` vs `<invalid-payload/>`) per §7.1.3.5.
    let Some(payload) = item.payload.as_ref() else {
        return Err(XmppError::pubsub_payload_required(Some(
            "XEP-0357 PubSub publish requires a notification payload".to_string(),
        )));
    };
    if payload.name() != "notification" || payload.ns() != NS_PUSH {
        return Err(XmppError::pubsub_invalid_payload(Some(
            "XEP-0357 PubSub publish payload must be notification in urn:xmpp:push:0".to_string(),
        )));
    }
    Ok(())
}

pub(super) fn push_pubsub_item_with_stable_id(item: &PubSubItem) -> PubSubItem {
    let mut item = item.clone();
    if item.id.is_none() {
        item.id = Some(uuid::Uuid::new_v4().to_string());
    }
    item
}

pub async fn ensure_xep0060_push_node(
    pubsub_storage: &Arc<dyn PubSubStorage>,
    push_service_jid: &BareJid,
    publisher: &BareJid,
    node: &str,
) -> Result<(), XmppError> {
    pubsub_storage
        .get_or_create_node(push_service_jid, node)
        .await?;
    pubsub_storage
        .update_node_config(push_service_jid, node, &NodeConfig::push_service())
        .await?;
    pubsub_storage
        .set_affiliation(push_service_jid, node, publisher, Affiliation::PublishOnly)
        .await?;
    Ok(())
}

impl DatabasePushServiceStore {
    pub(super) async fn ensure_xep0060_push_node_for_owner(
        &self,
        owner_bare_jid: &BareJid,
        node: &str,
    ) -> Result<(), XmppError> {
        let Some(boundary) = &self.pubsub_boundary else {
            return Ok(());
        };
        let push_node = self
            .get_node(node)
            .await?
            .ok_or_else(|| XmppError::item_not_found(Some("Push node not found".to_string())))?;
        if push_node.owner_bare_jid != *owner_bare_jid {
            return Err(XmppError::forbidden(Some(
                "Push node belongs to another owner".to_string(),
            )));
        }
        if push_node.status != PushNodeStatus::Active {
            return Err(XmppError::item_not_found(Some(
                "Push node not active".to_string(),
            )));
        }
        ensure_xep0060_push_node(
            &boundary.storage,
            &boundary.service_jid,
            owner_bare_jid,
            node,
        )
        .await
    }

    pub(super) async fn persist_xep0060_publish_if_configured(
        &self,
        push_service_jid: Option<&str>,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
    ) -> Result<(), XmppError> {
        let Some(boundary) = &self.pubsub_boundary else {
            return Ok(());
        };
        let Some(push_service_jid) = push_service_jid else {
            return Ok(());
        };
        let parsed_service_jid: BareJid = push_service_jid.parse().map_err(|error| {
            XmppError::bad_request(Some(format!("Invalid Push Service JID: {error}")))
        })?;
        if parsed_service_jid != boundary.service_jid {
            return Err(XmppError::bad_request(Some(
                "XEP-0357 Push Service publish target does not match configured service"
                    .to_string(),
            )));
        }
        validate_xep0357_notification(item)?;
        let can_publish = crate::pubsub_authz::can_publish(
            &boundary.storage,
            &boundary.service_jid,
            node,
            publisher,
            false,
        )
        .await?;
        if !can_publish {
            return Err(XmppError::forbidden(Some(
                "Publisher is not affiliated to publish to the Push Service PubSub node"
                    .to_string(),
            )));
        }
        boundary
            .storage
            .publish_item(&boundary.service_jid, node, item, Some(publisher), false)
            .await?;
        Ok(())
    }

    pub(super) async fn ensure_xep0060_publish_item_backing(
        &self,
        job: &PushPublishJob,
    ) -> Result<(), XmppError> {
        let Some(boundary) = &self.pubsub_boundary else {
            return Ok(());
        };
        let Some(push_service_jid) = job.push_service_jid() else {
            return Ok(());
        };
        let parsed_service_jid: BareJid = push_service_jid.parse().map_err(|error| {
            XmppError::bad_request(Some(format!("Invalid Push Service JID: {error}")))
        })?;
        if parsed_service_jid != boundary.service_jid {
            return Err(XmppError::bad_request(Some(
                "Push publish job service does not match configured Push Service".to_string(),
            )));
        }
        let items = boundary
            .storage
            .get_items(
                &boundary.service_jid,
                job.node(),
                Some(1),
                &[job.item_id().to_string()],
            )
            .await?;
        let item = items.into_iter().next().ok_or_else(|| {
            XmppError::item_not_found(Some(
                "Push publish job has no durable XEP-0060 PubSub item".to_string(),
            ))
        })?;
        let payload = item
            .payload_xml
            .as_deref()
            .ok_or_else(|| {
                XmppError::bad_request(Some("Stored PubSub item has no payload".to_string()))
            })?
            .parse::<Element>()
            .map_err(|error| {
                XmppError::bad_request(Some(format!(
                    "Stored PubSub payload is invalid XML: {error}"
                )))
            })?;
        validate_xep0357_notification(&PubSubItem::new(Some(item.id), Some(payload)))
    }
}
