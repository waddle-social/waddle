//! Trusted user-server publish entry points: direct fan-out publish,
//! registered-service publish, enqueue-only publish, and the XEP-0357
//! PubSub IQ boundary.

use jid::BareJid;
use minidom::Element;
use waddle_xmpp::pubsub::{PubSubItem, PubSubRequest};
use waddle_xmpp::XmppError;
use xmpp_parsers::iq::Iq;

use super::publish_jobs::MAX_DELIVERY_ATTEMPTS_PER_NODE;
use super::pubsub_backing::push_pubsub_item_with_stable_id;
use super::store::DatabasePushServiceStore;
use super::types::{PushFanoutResult, PushPublishJobEnqueue};

impl DatabasePushServiceStore {
    /// Enqueue and immediately try a trusted user-server XEP-0357 publish job.
    ///
    /// Client stanza ingress MUST NOT call this directly. XEP-0357 warns Push
    /// Services not to accept publishes from third-party client full JIDs; the
    /// caller is expected to be the durable user-server notification publisher.
    pub async fn publish_notification_from_user_server(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
    ) -> Result<PushFanoutResult, XmppError> {
        self.publish_notification_from_user_server_with_publish_options(node, item, publisher, None)
            .await
    }

    pub async fn publish_notification_from_user_server_with_publish_options(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        publish_options: Option<&Element>,
    ) -> Result<PushFanoutResult, XmppError> {
        self.publish_notification_from_user_server_with_retention_limit(
            node,
            item,
            publisher,
            None,
            publish_options,
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
        )
        .await
    }

    pub async fn publish_registered_notification_from_user_server_with_publish_options(
        &self,
        push_service_jid: &str,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        publish_options: Option<&Element>,
    ) -> Result<PushFanoutResult, XmppError> {
        self.publish_notification_from_user_server_with_retention_limit(
            node,
            item,
            publisher,
            Some(push_service_jid),
            publish_options,
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
        )
        .await
    }

    /// Accept host-approved provider work under a private delivery identity.
    /// Queue acceptance freezes its payload/options; the separate versioned
    /// PubSub projection cannot overwrite a newer wire publication on replay.
    /// Neither queue nor PubSub acceptance proves a provider accepted a send.
    pub(crate) async fn enqueue_registered_notification_from_user_server_with_publish_options(
        &self,
        push_service_jid: &BareJid,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        publish_options: Option<&Element>,
        delivery_id: uuid::Uuid,
    ) -> Result<PushPublishJobEnqueue, XmppError> {
        let item = push_pubsub_item_with_stable_id(item);
        let accepted = self
            .enqueue_canonical_notification_publish_job(
                node,
                &item,
                publisher,
                push_service_jid,
                publish_options,
                delivery_id,
            )
            .await?;
        self.complete_versioned_publish_backing(&accepted.job_id.to_string())
            .await?;
        Ok(accepted)
    }

    pub async fn publish_xep0357_pubsub_iq_from_user_server(
        &self,
        push_service_jid: &str,
        iq: &Iq,
        publisher: &BareJid,
    ) -> Result<PushFanoutResult, XmppError> {
        if !matches!(iq, Iq::Set { .. }) {
            return Err(XmppError::bad_request(Some(
                "XEP-0357 Push Service publish requires an IQ set".to_string(),
            )));
        }
        // RFC 6120 §8.1.1.1: stanzas routed to a peer entity MUST
        // carry both `from` and `to`. The previous `is_some_and`
        // shape silently accepted IQs with neither, which would
        // pass-through an addressed-less stanza into the publish
        // path. Require both and verify they match the trusted
        // caller-supplied publisher / service JID.
        let Some(from) = iq.from() else {
            return Err(XmppError::bad_request(Some(
                "XEP-0357 Push Service publish IQ missing `from`".to_string(),
            )));
        };
        if from.to_bare() != *publisher {
            return Err(XmppError::forbidden(Some(
                "XEP-0357 Push Service publish sender does not match publisher".to_string(),
            )));
        }
        let Some(to) = iq.to() else {
            return Err(XmppError::bad_request(Some(
                "XEP-0357 Push Service publish IQ missing `to`".to_string(),
            )));
        };
        if to.to_string() != push_service_jid {
            return Err(XmppError::bad_request(Some(
                "XEP-0357 Push Service publish target does not match service".to_string(),
            )));
        }

        match waddle_xmpp::pubsub::parse_pubsub_iq(iq)
            .map_err(|error| XmppError::bad_request(Some(error.to_string())))?
        {
            PubSubRequest::Publish {
                node,
                item,
                publish_options,
            } => {
                self.publish_registered_notification_from_user_server_with_publish_options(
                    push_service_jid,
                    &node,
                    &item,
                    publisher,
                    publish_options.as_deref(),
                )
                .await
            }
            _ => Err(XmppError::bad_request(Some(
                "XEP-0357 Push Service publish requires a PubSub publish request".to_string(),
            ))),
        }
    }

    async fn publish_notification_from_user_server_with_retention_limit(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        push_service_jid: Option<&str>,
        publish_options: Option<&Element>,
        retention_limit: i64,
    ) -> Result<PushFanoutResult, XmppError> {
        let item = push_pubsub_item_with_stable_id(item);
        let enqueue = self
            .enqueue_notification_publish_job_from_user_server_with_publish_options(
                node,
                &item,
                publisher,
                push_service_jid,
                publish_options,
            )
            .await?;

        self.complete_versioned_publish_backing(&enqueue.job_id.to_string())
            .await?;
        match self
            .process_publish_job_with_retention_limit(&enqueue.job_id.to_string(), retention_limit)
            .await
        {
            Ok(Some(result)) => Ok(result),
            Ok(None) => Ok(PushFanoutResult {
                item_id: enqueue.item_id,
                attempted_devices: 0,
            }),
            Err(error) => {
                self.record_publish_job_failure_by_id(
                    &enqueue.job_id.to_string(),
                    &error.to_string(),
                )
                .await?;
                Err(error)
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::push_service::publish_jobs::{
        PUBLISH_JOB_CLAIM_TIMEOUT_MS, PUBLISH_JOB_STATUS_IN_PROGRESS,
    };
    use crate::push_service::test_support::{notification_item, owner, store};
    use crate::push_service::{PushDevicePlatform, PushDeviceRegistration};

    #[tokio::test]
    async fn publish_notification_prunes_attempts_on_publish_path() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("push node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("web-1", node.node(), PushDevicePlatform::Web, "test"),
            )
            .await
            .expect("device");

        let retention_limit = 3;
        store
            .publish_notification_from_user_server_with_retention_limit(
                node.node(),
                &notification_item("item-0"),
                &owner,
                None,
                None,
                retention_limit,
            )
            .await
            .expect("first publish");
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
        for idx in 1..=retention_limit {
            store
                .publish_notification_from_user_server_with_retention_limit(
                    node.node(),
                    &notification_item(&format!("item-{idx}")),
                    &owner,
                    None,
                    None,
                    retention_limit,
                )
                .await
                .expect("publish over retention limit");
        }

        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");

        assert_eq!(attempts.len(), retention_limit as usize);
        assert!(
            attempts.iter().all(|attempt| attempt.item_id() != "item-0"),
            "oldest publish-path attempt should be pruned"
        );
    }

    #[tokio::test]
    async fn wire_publication_does_not_reuse_another_acceptance_claim() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("dev-1", node.node(), PushDevicePlatform::Fcm, "test"),
            )
            .await
            .expect("device");
        store
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("recover-direct-claim"),
                &owner,
            )
            .await
            .expect("enqueue");
        store
            .execute(
                r#"
                UPDATE push_publish_jobs
                SET status = ?,
                    claimed_at_ms = ?,
                    updated_at_ms = ?
                WHERE node = ? AND item_id = ?
                "#,
                crate::db_params![
                    PUBLISH_JOB_STATUS_IN_PROGRESS,
                    crate::time::now_ms() - PUBLISH_JOB_CLAIM_TIMEOUT_MS - 1,
                    crate::time::now_ms() - PUBLISH_JOB_CLAIM_TIMEOUT_MS - 1,
                    node.node(),
                    "recover-direct-claim",
                ],
            )
            .await
            .expect("force stale claim");

        let result = store
            .publish_notification_from_user_server(
                node.node(),
                &notification_item("recover-direct-claim"),
                &owner,
            )
            .await
            .expect("direct publish retry");
        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");

        assert_eq!(result.item_id(), "recover-direct-claim");
        assert_eq!(result.attempted_devices(), 1);
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].item_id(), "recover-direct-claim");
        assert!(queued.is_empty());
        let mut rows = store
            .query(
                "SELECT COUNT(*) FROM push_publish_jobs WHERE item_id = ? AND status = ?",
                crate::db_params!["recover-direct-claim", PUBLISH_JOB_STATUS_IN_PROGRESS],
            )
            .await
            .expect("original claim");
        assert_eq!(
            rows.next()
                .await
                .expect("row")
                .expect("count")
                .get::<i64>(0)
                .expect("value"),
            1,
            "the new wire publication cannot claim another acceptance's expired lease"
        );
    }
}
