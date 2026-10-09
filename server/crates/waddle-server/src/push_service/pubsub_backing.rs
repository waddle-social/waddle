//! XEP-0060 PubSub backing for the Push Service: node provisioning on
//! the configured PubSub boundary, durable publish persistence, and
//! XEP-0357 `<notification/>` payload validation.

use std::sync::Arc;

use jid::BareJid;
use minidom::Element;
use waddle_xmpp::pubsub::{
    Affiliation, NodeConfig, PubSubItem, PubSubStorage, PublicationNode, PublicationVersion,
    VersionedPublishResult,
};
use waddle_xmpp::xep::xep0357::NS_PUSH;
use waddle_xmpp::XmppError;

use super::store::DatabasePushServiceStore;
use super::types::{PushBackingState, PushNodeStatus};

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

    /// Complete the independent PubSub projection outside provider/queue
    /// transactions. Its revision/token contract rejects delayed old writes;
    /// it proves no provider accepted the frozen queued notification.
    pub(super) async fn complete_versioned_publish_backing(
        &self,
        job_id: &str,
    ) -> Result<(), XmppError> {
        let Some(job) = self.load_publish_job(job_id).await? else {
            return Err(XmppError::internal("publish acceptance missing"));
        };
        if job.backing_state != PushBackingState::Pending
            || matches!(job.status(), "published" | "failed")
        {
            return Ok(());
        }
        let state = if let (Some(boundary), Some(service)) =
            (&self.pubsub_boundary, job.push_service_jid())
        {
            if service != &boundary.service_jid {
                return Err(XmppError::bad_request(Some(
                    "push service backing target mismatch".to_string(),
                )));
            }
            if !crate::pubsub_authz::can_publish(
                &boundary.storage,
                &boundary.service_jid,
                job.node(),
                job.owner_bare_jid(),
                false,
            )
            .await?
            {
                return Err(XmppError::forbidden(Some(
                    "publisher not affiliated to push node".to_string(),
                )));
            }
            let db = self.database();
            let conn = db
                .guard()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            let mut rows = conn
                .query(
                    "SELECT payload_xml FROM push_publish_jobs WHERE job_id = ?",
                    crate::db_params![job_id],
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            let row = rows
                .next()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?
                .ok_or_else(|| XmppError::internal("accepted payload missing"))?;
            let payload: String = row
                .get(0)
                .map_err(|error| XmppError::internal(error.to_string()))?;
            let payload: Element = payload
                .parse()
                .map_err(|_| XmppError::internal("accepted payload malformed"))?;
            drop(rows);
            drop(conn);
            let token = uuid::Uuid::parse_str(job_id)
                .map_err(|_| XmppError::internal("invalid publication token"))?;
            let version = PublicationVersion::new(job.publication_order, token)
                .map_err(|error| error.into_xmpp_error())?;
            let node = PublicationNode::new(job.node()).map_err(|error| error.into_xmpp_error())?;
            let item = PubSubItem::new(Some(job.item_id().to_string()), Some(payload));
            match boundary
                .storage
                .publish_push_item_versioned(
                    &boundary.service_jid,
                    job.owner_bare_jid(),
                    &node,
                    &item,
                    version,
                )
                .await
                .map_err(|error| error.into_xmpp_error())?
            {
                VersionedPublishResult::Applied(_) | VersionedPublishResult::AlreadyApplied => {
                    "published"
                }
                VersionedPublishResult::Superseded => "superseded",
            }
        } else {
            "not-configured"
        };
        self.execute("UPDATE push_publish_jobs SET backing_state = ?, backing_published_at_ms = ? WHERE job_id = ? AND backing_state = 'pending'", crate::db_params![state, crate::time::now_ms(), job_id]).await?;
        Ok(())
    }
}
