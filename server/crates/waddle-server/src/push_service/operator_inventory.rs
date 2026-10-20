//! Exact-target inventory for authenticated operator commands. Never exported
//! as metric attributes: node identifiers and owner JIDs stay on the XMPP path.

use jid::BareJid;
use waddle_xmpp::XmppError;

use super::nodes::MAX_NODE_ID_LEN;
use super::publish_jobs::{MAX_PUBLISH_JOBS_PER_NODE, PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS};
use super::DatabasePushServiceStore;

/// A single opaque push node, parsed before crossing the operator boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOperatorNode(String);

impl PushOperatorNode {
    pub fn parse(value: &str) -> Result<Self, XmppError> {
        if value.is_empty() || value.len() > MAX_NODE_ID_LEN || value.chars().any(char::is_control)
        {
            return Err(XmppError::bad_request(Some(
                "Invalid exact push node".into(),
            )));
        }
        Ok(Self(value.to_owned()))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PushOperatorNodeStatus {
    Missing,
    Active,
    Disabled,
}

impl PushOperatorNodeStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Active => "active",
            Self::Disabled => "disabled",
        }
    }
}

/// Point-in-time counts for one owner/node; no notification content or secrets.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PushOperatorInventory {
    pub status: PushOperatorNodeStatus,
    pub provider_retained: i64,
    pub provider_parked: i64,
    pub provider_at_budget: i64,
    pub provider_unresolved_failed: i64,
    pub outbox_retained: i64,
    pub outbox_parked: i64,
    pub outbox_at_budget: i64,
    pub outbox_unresolved_failed: i64,
    pub provider_capacity: i64,
}

impl DatabasePushServiceStore {
    pub async fn operator_inventory(
        &self,
        owner: &BareJid,
        node: &PushOperatorNode,
    ) -> Result<PushOperatorInventory, XmppError> {
        let (lineage, foundation) = super::observability::ancestry_tables(&self.db)
            .await
            .map_err(inventory_error)?;
        let conn = self.db.guard().await.map_err(inventory_error)?;
        // Standalone push stores intentionally have no outbox tables. Probe
        // schema only; the actual inventory is one coherent SQL statement.
        let probe = if self.db.driver() == crate::db::DatabaseDriver::Postgres {
            "SELECT CAST(CASE WHEN to_regclass('notification_outbox') IS NULL THEN 0 ELSE 1 END AS BIGINT)"
        } else {
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = 'notification_outbox'"
        };
        let mut rows = conn.query(probe, ()).await.map_err(inventory_error)?;
        let row = rows
            .next()
            .await
            .map_err(inventory_error)?
            .ok_or_else(|| XmppError::internal("Missing push inventory schema probe"))?;
        let has_outbox = row.get::<i64>(0).map_err(inventory_error)? != 0;
        drop(rows);
        let provider_lineage = if lineage {
            "EXISTS(SELECT 1 FROM notification_outbox_lineage l WHERE l.job_id = p.ancestry_job_id AND l.settled_at_ms IS NULL)"
        } else {
            "FALSE"
        };
        let provider_foundation = if foundation {
            "EXISTS(SELECT 1 FROM ingress_effect_descendants d WHERE d.descendant_key = p.ancestry_job_id AND d.settled_at IS NULL)"
        } else {
            "FALSE"
        };
        let outbox_lineage = if lineage {
            "EXISTS(SELECT 1 FROM notification_outbox_lineage l WHERE l.job_id = o.job_id AND l.settled_at_ms IS NULL)"
        } else {
            "FALSE"
        };
        let outbox_foundation = if foundation {
            "EXISTS(SELECT 1 FROM ingress_effect_descendants d WHERE d.descendant_key = o.job_id AND d.settled_at IS NULL)"
        } else {
            "FALSE"
        };
        let outbox = if has_outbox {
            // The budget is the historical deployment threshold, independent
            // of future policy changes: operators need to identify old backlog.
            format!("(SELECT COUNT(*) FROM notification_outbox WHERE recipient_bare_jid = target.owner_jid AND node = target.node_id), \
             (SELECT COUNT(*) FROM notification_outbox WHERE recipient_bare_jid = target.owner_jid AND node = target.node_id AND status = 'failed' AND queue_acceptance_may_exist = 1 AND published_at_ms IS NULL), \
             (SELECT COUNT(*) FROM notification_outbox WHERE recipient_bare_jid = target.owner_jid AND node = target.node_id AND status IN ('queued', 'in-progress') AND queue_acceptance_may_exist = 1 AND attempt_count + policy_error_count >= 5), \
             (SELECT COUNT(*) FROM notification_outbox o WHERE recipient_bare_jid = target.owner_jid AND node = target.node_id AND status = 'failed' AND queue_acceptance_may_exist = 0 AND ({outbox_lineage} OR {outbox_foundation}))")
        } else {
            "CAST(0 AS BIGINT), CAST(0 AS BIGINT), CAST(0 AS BIGINT), CAST(0 AS BIGINT)".to_owned()
        };
        let sql = format!(
            "WITH target AS (SELECT ? AS owner_jid, ? AS node_id, ? AS budget) SELECT \
             (SELECT status FROM push_nodes WHERE owner_bare_jid = target.owner_jid AND node = target.node_id), \
             (SELECT COUNT(*) FROM push_publish_jobs WHERE owner_bare_jid = target.owner_jid AND node = target.node_id), \
             (SELECT COUNT(*) FROM push_publish_jobs WHERE owner_bare_jid = target.owner_jid AND node = target.node_id AND status = 'failed' AND uncertain_send = 1 AND terminal_disposition IS NULL), \
             (SELECT COUNT(*) FROM push_publish_jobs WHERE owner_bare_jid = target.owner_jid AND node = target.node_id AND status IN ('queued', 'in-progress') AND attempt_count >= target.budget), \
             (SELECT COUNT(*) FROM push_publish_jobs p WHERE owner_bare_jid = target.owner_jid AND node = target.node_id AND status = 'failed' AND uncertain_send = 0 AND ({provider_lineage} OR {provider_foundation})), {outbox} FROM target"
        );
        let mut rows = conn
            .query(
                &sql,
                crate::db_params![
                    owner.to_string(),
                    node.as_str(),
                    PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS
                ],
            )
            .await
            .map_err(inventory_error)?;
        let row = rows
            .next()
            .await
            .map_err(inventory_error)?
            .ok_or_else(|| XmppError::internal("Missing push inventory result"))?;
        let status = match row
            .get::<Option<String>>(0)
            .map_err(inventory_error)?
            .as_deref()
        {
            None => PushOperatorNodeStatus::Missing,
            Some("active") => PushOperatorNodeStatus::Active,
            Some("disabled") => PushOperatorNodeStatus::Disabled,
            Some(_) => return Err(XmppError::internal("Invalid push node inventory status")),
        };
        Ok(PushOperatorInventory {
            status,
            provider_retained: row.get(1).map_err(inventory_error)?,
            provider_parked: row.get(2).map_err(inventory_error)?,
            provider_at_budget: row.get(3).map_err(inventory_error)?,
            provider_unresolved_failed: row.get(4).map_err(inventory_error)?,
            outbox_retained: row.get(5).map_err(inventory_error)?,
            outbox_parked: row.get(6).map_err(inventory_error)?,
            outbox_at_budget: row.get(7).map_err(inventory_error)?,
            outbox_unresolved_failed: row.get(8).map_err(inventory_error)?,
            provider_capacity: MAX_PUBLISH_JOBS_PER_NODE,
        })
    }
}

fn inventory_error(error: impl std::fmt::Display) -> XmppError {
    XmppError::internal(format!("Push operator inventory failed: {error}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::push_service::test_support::{notification_item, owner, store};

    #[tokio::test]
    async fn inventory_counts_exact_owner_node_without_outbox_schema() {
        let push = store().await;
        let node = push
            .ensure_node(&owner(), "operator-inventory")
            .await
            .expect("node");
        let target = PushOperatorNode::parse(node.node()).expect("target");
        let accepted = push
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("parked"),
                &owner(),
            )
            .await
            .expect("enqueue");
        push.execute("UPDATE push_publish_jobs SET status = 'failed', uncertain_send = 1, attempt_count = 24 WHERE job_id = ?",
            crate::db_params![accepted.job_id().to_string()]).await.expect("park");
        let legacy = push
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("legacy-budget"),
                &owner(),
            )
            .await
            .expect("legacy enqueue");
        push.execute(
            "UPDATE push_publish_jobs SET attempt_count = 24 WHERE job_id = ?",
            crate::db_params![legacy.job_id().to_string()],
        )
        .await
        .expect("legacy attempt budget");
        let inventory = push
            .operator_inventory(&owner(), &target)
            .await
            .expect("inventory");
        assert_eq!(inventory.status, PushOperatorNodeStatus::Active);
        assert_eq!(inventory.provider_retained, 2);
        assert_eq!(inventory.provider_parked, 1);
        assert_eq!(inventory.provider_unresolved_failed, 0);
        assert_eq!(inventory.provider_at_budget, 1);
        assert_eq!(inventory.outbox_retained, 0);
        assert_eq!(inventory.outbox_unresolved_failed, 0);
        assert_eq!(inventory.provider_capacity, 10_000);
        let foreign: BareJid = "other@example.com".parse().expect("owner");
        let inventory = push
            .operator_inventory(&foreign, &target)
            .await
            .expect("foreign inventory");
        assert_eq!(inventory.status, PushOperatorNodeStatus::Missing);
        assert_eq!(inventory.provider_retained, 0);
        assert_eq!(inventory.provider_parked, 0);
    }

    #[tokio::test]
    async fn inventory_includes_parked_outbox_without_provider_acceptance() {
        let push = store().await;
        crate::notification_outbox::NotificationOutboxStore::new(push.db.clone())
            .await
            .expect("outbox schema");
        let node = PushOperatorNode::parse("outbox-only-node").expect("node");
        push.execute("INSERT INTO notification_outbox (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, class, message_count, context_xml, status, attempt_count, queue_acceptance_may_exist, created_at_ms, updated_at_ms) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 1, ?, 'failed', 5, 1, 1, 1)",
            crate::db_params!["parked-outbox", owner().to_string(), "push.example.com", node.as_str(), "bob@example.com", "bob@example.com", "[]", "dm", "<notification/>"])
            .await.expect("parked outbox");
        let inventory = push
            .operator_inventory(&owner(), &node)
            .await
            .expect("inventory");
        assert_eq!(inventory.status, PushOperatorNodeStatus::Missing);
        assert_eq!(inventory.provider_retained, 0);
        assert_eq!(inventory.outbox_retained, 1);
        assert_eq!(inventory.outbox_parked, 1);
        assert_eq!(inventory.outbox_at_budget, 0);
    }

    async fn inventory_tracks_actual_unsettled_known_failures(
        fixture: crate::ingress::test_support::IngressFixture,
    ) {
        use crate::ingress_uow::{
            CanonicalMessageRepository, EffectDescendantRepository, EffectIntentRepository,
        };
        use waddle_xmpp::ingress::{
            IngressEffectIntent, MessageKey, NotificationActivityMutation, SemanticDigest,
        };

        let push = DatabasePushServiceStore::new(fixture.db.clone())
            .await
            .expect("provider");
        crate::notification_outbox::NotificationOutboxStore::new(fixture.db.clone())
            .await
            .expect("outbox");
        let node = push
            .ensure_node(&owner(), "operator-known-failure")
            .await
            .expect("node");
        let target = PushOperatorNode::parse(node.node()).expect("target");
        let lineage_job = uuid::Uuid::new_v4();
        let foundation_job = uuid::Uuid::new_v4();
        let settled_job = uuid::Uuid::new_v4();
        let no_parent_job = uuid::Uuid::new_v4();
        let payload = String::from(&notification_item("inventory").payload.expect("payload"));
        for id in [lineage_job, foundation_job, settled_job, no_parent_job] {
            fixture.execute("INSERT INTO notification_outbox (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, class, message_count, context_xml, status, queue_acceptance_may_exist, created_at_ms, updated_at_ms) VALUES (?, ?, 'push.example.com', ?, 'bob@example.com', 'bob@example.com', '[]', 'dm', 1, ?, 'failed', 0, 1, 1)",
                crate::db_params![id.to_string(), owner().to_string(), node.node(), &payload]).await;
            fixture.execute("INSERT INTO push_publish_jobs (job_id, owner_bare_jid, node, item_id, payload_xml, ancestry_job_id, status, uncertain_send, upstream_completed, created_at_ms, updated_at_ms) VALUES (?, ?, ?, ?, ?, ?, 'failed', 0, 1, 1, 1)",
                crate::db_params![format!("provider-{id}"), owner().to_string(), node.node(), id.to_string(), &payload, id.to_string()]).await;
        }
        fixture.execute("INSERT INTO notification_outbox_lineage (candidate_delivery_id, job_id, settled_at_ms) VALUES (?, ?, NULL), (?, ?, NULL), (?, ?, 1)",
            crate::db_params![uuid::Uuid::new_v4().to_string(), lineage_job.to_string(), uuid::Uuid::new_v4().to_string(), lineage_job.to_string(), uuid::Uuid::new_v4().to_string(), settled_job.to_string()]).await;
        let message = MessageKey::new();
        let intent = IngressEffectIntent::NotificationActivityPreview {
            owner: owner(),
            mutation: NotificationActivityMutation::NotificationCandidate {
                conversation: "bob@example.com".parse().expect("conversation"),
                archive_stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
                    "inventory",
                    owner().into(),
                ),
                outcome: waddle_xmpp::ingress::NotificationCandidateOutcome::Inserted,
            },
        };
        let mut tx = fixture.uow.begin().await.expect("canonical transaction");
        CanonicalMessageRepository::record_message(
            &mut tx,
            message,
            &SemanticDigest::from_storage(1, [1; 32]).expect("digest"),
            None,
        )
        .await
        .expect("message");
        EffectIntentRepository::reconcile(&mut tx, message, std::slice::from_ref(&intent), false)
            .await
            .expect("intent");
        EffectDescendantRepository::attach(
            &mut tx,
            message,
            &intent.semantic_key(),
            foundation_job,
        )
        .await
        .expect("descendant");
        tx.commit().await.expect("commit");

        let before = push
            .operator_inventory(&owner(), &target)
            .await
            .expect("inventory");
        assert_eq!(
            before.provider_unresolved_failed, 2,
            "lineage and Foundation each retain custody; duplicate parents count once"
        );
        assert_eq!(before.outbox_unresolved_failed, 2);
        let foreign: BareJid = "other@example.com".parse().expect("owner");
        let unrelated = push
            .operator_inventory(&foreign, &target)
            .await
            .expect("foreign owner");
        assert_eq!(unrelated.provider_unresolved_failed, 0);
        assert_eq!(unrelated.outbox_unresolved_failed, 0);
        push.disable_nodes_for_owner(&owner(), Some(node.node()))
            .await
            .expect("retire");
        let after = push
            .operator_inventory(&owner(), &target)
            .await
            .expect("retired inventory");
        assert_eq!(after.provider_unresolved_failed, 0);
        assert_eq!(after.outbox_unresolved_failed, 0);
        assert_eq!(after.status, PushOperatorNodeStatus::Disabled);
        fixture.close().await;
    }

    #[tokio::test]
    async fn sqlite_inventory_tracks_actual_unsettled_known_failures() {
        inventory_tracks_actual_unsettled_known_failures(
            crate::ingress::test_support::IngressFixture::sqlite().await,
        )
        .await;
    }

    #[tokio::test]
    async fn postgres_inventory_tracks_actual_unsettled_known_failures() {
        if let Some(fixture) =
            crate::ingress::test_support::IngressFixture::postgres("operator_inventory").await
        {
            inventory_tracks_actual_unsettled_known_failures(fixture).await;
        }
    }
}
