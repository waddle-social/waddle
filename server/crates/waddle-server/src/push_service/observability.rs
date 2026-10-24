//! Current durable queue state, sampled independently of provider activity.

use super::publish_jobs::MAX_PUBLISH_JOBS_PER_NODE;
use super::DatabasePushServiceStore;
use crate::db::{Database, DatabaseDriver, DatabaseError};
use waddle_xmpp::telemetry::attributes::PushQueue;
use waddle_xmpp::XmppError;

/// A single database statement observes all provider inventory dimensions.
/// Separate outbox/provider stores are not claimed to share a snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushProviderQueueObservation {
    pub parked: u64,
    pub unresolved_failed: u64,
    pub retained_max_per_node: u64,
    pub near_capacity_nodes: u64,
    /// Unknown for standalone stores or an absent host registration schema.
    pub local_registrations: Option<PushLocalRegistrationObservation>,
}

/// Enabled registrations for the configured service, not historical outbox
/// targets. Counts reveal no owner, node, job or message identifiers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushLocalRegistrationObservation {
    pub missing_nodes: u64,
    pub inactive_nodes: u64,
}

impl PushProviderQueueObservation {
    pub(crate) fn record(self) {
        let snapshot_at = crate::time::now_ms().max(0) as u64 / 1_000;
        waddle_xmpp::telemetry::reliability::record_push_provider_capacity(
            self.retained_max_per_node,
            self.near_capacity_nodes,
        );
        if let Some(registrations) = self.local_registrations {
            waddle_xmpp::telemetry::reliability::record_push_registration_snapshot(
                registrations.missing_nodes,
                registrations.inactive_nodes,
                snapshot_at,
            );
        }
        waddle_xmpp::telemetry::reliability::record_push_queue_snapshot(
            PushQueue::Provider,
            self.parked,
            self.unresolved_failed,
            snapshot_at,
        );
    }
}

/// Standalone provider databases used by embedders need not have Foundation
/// or outbox tables. Probe schema presence before preparing a query that names
/// optional tables; all inventory values still come from one aggregate SELECT.
pub(crate) async fn ancestry_tables(db: &Database) -> Result<(bool, bool), DatabaseError> {
    let conn = db.guard().await?;
    let sql = match db.driver() {
        DatabaseDriver::Postgres => {
            "SELECT to_regclass('notification_outbox_lineage') IS NOT NULL, to_regclass('ingress_effect_descendants') IS NOT NULL"
        }
        DatabaseDriver::Sqlite => {
            "SELECT EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'notification_outbox_lineage'), EXISTS(SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ingress_effect_descendants')"
        }
    };
    let mut rows = conn.query(sql, ()).await?;
    let row = rows.next().await?.ok_or_else(|| {
        DatabaseError::QueryFailed("push ancestry schema probe returned no row".into())
    })?;
    Ok((row.get(0)?, row.get(1)?))
}

impl DatabasePushServiceStore {
    pub async fn queue_observation(&self) -> Result<PushProviderQueueObservation, XmppError> {
        let (lineage, foundation) = ancestry_tables(&self.db)
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let lineage_guard = if lineage {
            "EXISTS(SELECT 1 FROM notification_outbox_lineage l WHERE l.job_id = p.ancestry_job_id AND l.settled_at_ms IS NULL)"
        } else {
            "FALSE"
        };
        let foundation_guard = if foundation {
            "EXISTS(SELECT 1 FROM ingress_effect_descendants d WHERE d.descendant_key = p.ancestry_job_id AND d.settled_at IS NULL)"
        } else {
            "FALSE"
        };
        let registration_schema = if self.pubsub_boundary.is_some() {
            let probe = match self.db.driver() {
                DatabaseDriver::Postgres => "SELECT 1 WHERE to_regclass('push_registrations') IS NOT NULL",
                DatabaseDriver::Sqlite => "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'push_registrations'",
            };
            self.query(probe, ())
                .await?
                .next()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?
                .is_some()
        } else {
            false
        };
        let local_service = self
            .pubsub_boundary
            .as_ref()
            .filter(|_| registration_schema);
        let mut params = crate::db_params![];
        let registrations = if let Some(boundary) = local_service {
            params.push(crate::db::Value::from(boundary.service_jid.to_string()));
            "SELECT CAST(COALESCE(SUM(CASE WHEN n.node IS NULL THEN 1 ELSE 0 END), 0) AS BIGINT) AS missing_nodes,
                CAST(COALESCE(SUM(CASE WHEN n.node IS NOT NULL AND n.status <> 'active' THEN 1 ELSE 0 END), 0) AS BIGINT) AS inactive_nodes
             FROM push_registrations r LEFT JOIN push_nodes n ON n.node = r.node AND n.owner_bare_jid = r.owner_bare_jid
             WHERE r.push_service_jid = ? AND r.status = 'enabled'"
        } else {
            "SELECT 0 AS missing_nodes, 0 AS inactive_nodes"
        };
        params.push(crate::db::Value::from(MAX_PUBLISH_JOBS_PER_NODE * 9 / 10));
        let sql = format!(
            "WITH node_inventory AS (SELECT node, COUNT(*) AS retained,
                SUM(CASE WHEN status = 'failed' AND uncertain_send = 1 AND terminal_disposition IS NULL THEN 1 ELSE 0 END) AS parked,
                SUM(CASE WHEN status = 'failed' AND uncertain_send = 0 AND ({lineage_guard} OR {foundation_guard}) THEN 1 ELSE 0 END) AS unresolved_failed
                FROM push_publish_jobs p GROUP BY node), registration_inventory AS ({registrations})
             SELECT CAST(COALESCE(SUM(parked), 0) AS BIGINT), CAST(COALESCE(SUM(unresolved_failed), 0) AS BIGINT),
                COALESCE(MAX(retained), 0), COALESCE(SUM(CASE WHEN retained >= ? THEN 1 ELSE 0 END), 0),
                (SELECT missing_nodes FROM registration_inventory), (SELECT inactive_nodes FROM registration_inventory)
             FROM node_inventory"
        );
        let mut rows = self.query(&sql, params).await?;
        let row = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
            .ok_or_else(|| XmppError::internal("push inventory aggregate missing"))?;
        let count = |column| -> Result<u64, XmppError> {
            let value: i64 = row
                .get(column)
                .map_err(|error| XmppError::internal(error.to_string()))?;
            u64::try_from(value).map_err(|_| XmppError::internal("negative push inventory"))
        };
        Ok(PushProviderQueueObservation {
            parked: count(0)?,
            unresolved_failed: count(1)?,
            retained_max_per_node: count(2)?,
            near_capacity_nodes: count(3)?,
            local_registrations: if local_service.is_some() {
                Some(PushLocalRegistrationObservation {
                    missing_nodes: count(4)?,
                    inactive_nodes: count(5)?,
                })
            } else {
                None
            },
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ingress::test_support::IngressFixture;
    use crate::ingress_uow::{
        CanonicalMessageRepository, EffectDescendantRepository, EffectIntentRepository,
    };
    use crate::notification_outbox::{NotificationOutboxStore, PushOutboxQueueObservation};
    use crate::push_service::test_support::{notification_item, owner};
    use waddle_xmpp::ingress::{
        IngressEffectIntent, MessageKey, NotificationActivityMutation, SemanticDigest,
    };

    async fn push_queue_observation_tracks_retained_capacity_and_unsettled_lineage(
        fixture: IngressFixture,
    ) {
        let provider = DatabasePushServiceStore::try_new(fixture.db.clone())
            .await
            .expect("provider");
        let outbox = NotificationOutboxStore::try_new(fixture.db.clone())
            .await
            .expect("outbox");
        let owner = owner();
        let node = provider
            .ensure_node(&owner, "observability")
            .await
            .expect("node");
        let payload = String::from(&notification_item("snapshot").payload.expect("payload"));
        assert_eq!(
            provider.queue_observation().await.expect("empty provider"),
            PushProviderQueueObservation {
                parked: 0,
                unresolved_failed: 0,
                retained_max_per_node: 0,
                near_capacity_nodes: 0,
                local_registrations: None,
            }
        );
        assert_eq!(
            outbox.queue_observation().await.expect("empty outbox"),
            PushOutboxQueueObservation {
                parked: 0,
                unresolved_failed: 0,
            }
        );
        fixture.execute(
            "WITH RECURSIVE seq(n) AS (SELECT 1 UNION ALL SELECT n + 1 FROM seq WHERE n < 8999)
             INSERT INTO push_publish_jobs(job_id, owner_bare_jid, node, item_id, payload_xml, status, created_at_ms, updated_at_ms)
             SELECT 'snapshot-' || CAST(n AS TEXT), ?, ?, CAST(n AS TEXT), ?, 'published', 1, 1 FROM seq",
            crate::db_params![owner.to_string(), node.node(), &payload],
        ).await;
        // A published retention tail consumes the same cap as unresolved work.
        assert_eq!(
            provider.queue_observation().await.expect("below cap"),
            PushProviderQueueObservation {
                parked: 0,
                unresolved_failed: 0,
                retained_max_per_node: 8999,
                near_capacity_nodes: 0,
                local_registrations: None,
            }
        );
        fixture.execute(
            "INSERT INTO push_publish_jobs(job_id, owner_bare_jid, node, item_id, payload_xml, status, uncertain_send, upstream_completed, created_at_ms, updated_at_ms)
             VALUES ('parked', ?, ?, 'parked', ?, 'failed', 1, 1, 1, 1)",
            crate::db_params![owner.to_string(), node.node(), &payload],
        ).await;
        // Upstream completion does not prove the provider accepted a notification.
        assert_eq!(
            provider.queue_observation().await.expect("cap warning"),
            PushProviderQueueObservation {
                parked: 1,
                unresolved_failed: 0,
                retained_max_per_node: 9000,
                near_capacity_nodes: 1,
                local_registrations: None,
            }
        );
        fixture.execute(
            "INSERT INTO notification_outbox(job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, sender_jid, sender_jids, class, message_count, context_xml, status, queue_acceptance_may_exist, created_at_ms, updated_at_ms)
             VALUES ('known-pending', ?, 'push.example.com', ?, 'bob@example.com', 'bob@example.com', '[]', 'dm', 1, ?, 'failed', 0, 1, 1),
                    ('known-settled', ?, 'push.example.com', ?, 'bob@example.com', 'bob@example.com', '[]', 'dm', 1, ?, 'failed', 0, 1, 1),
                    ('parked', ?, 'push.example.com', ?, 'bob@example.com', 'bob@example.com', '[]', 'dm', 1, ?, 'failed', 1, 1, 1),
                    ('known-no-parent', ?, 'push.example.com', ?, 'bob@example.com', 'bob@example.com', '[]', 'dm', 1, ?, 'failed', 0, 1, 1)",
            crate::db_params![owner.to_string(),node.node(),&payload,owner.to_string(),node.node(),&payload,owner.to_string(),node.node(),&payload,owner.to_string(),node.node(),&payload],
        ).await;
        fixture.execute(
            "INSERT INTO notification_outbox_lineage(candidate_delivery_id, job_id, settled_at_ms)
             VALUES ('candidate-one', 'known-pending', NULL), ('candidate-two', 'known-settled', 1), ('candidate-three', 'known-pending', NULL)", (),
        ).await;
        fixture.execute(
            "UPDATE push_publish_jobs SET status = 'failed', ancestry_job_id = 'known-pending' WHERE job_id = 'snapshot-1'", (),
        ).await;
        fixture.execute(
            "UPDATE push_publish_jobs SET status = 'failed', ancestry_job_id = 'known-settled' WHERE job_id = 'snapshot-2'", (),
        ).await;
        assert_eq!(
            outbox.queue_observation().await.expect("outbox inventory"),
            PushOutboxQueueObservation {
                parked: 1,
                unresolved_failed: 1,
            }
        );
        assert_eq!(
            provider
                .queue_observation()
                .await
                .expect("provider inventory"),
            PushProviderQueueObservation {
                parked: 1,
                unresolved_failed: 1,
                retained_max_per_node: 9000,
                near_capacity_nodes: 1,
                local_registrations: None,
            }
        );
        // Foundation custody also exists without a notification lineage row.
        let descendant = uuid::Uuid::new_v4();
        let message = MessageKey::new();
        let intent = IngressEffectIntent::NotificationActivityPreview {
            owner: owner.clone(),
            mutation: NotificationActivityMutation::NotificationCandidate {
                conversation: "bob@example.com".parse().expect("conversation"),
                archive_stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
                    "snapshot",
                    owner.clone().into(),
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
        .expect("canonical message");
        EffectIntentRepository::reconcile(&mut tx, message, std::slice::from_ref(&intent), false)
            .await
            .expect("effect");
        EffectDescendantRepository::attach(&mut tx, message, &intent.semantic_key(), descendant)
            .await
            .expect("descendant");
        tx.commit().await.expect("canonical commit");
        fixture
            .execute(
                "UPDATE notification_outbox SET job_id = ? WHERE job_id = 'known-no-parent'",
                crate::db_params![descendant.to_string()],
            )
            .await;
        fixture.execute("UPDATE push_publish_jobs SET status = 'failed', ancestry_job_id = ? WHERE job_id = 'snapshot-3'", crate::db_params![descendant.to_string()]).await;
        assert_eq!(
            outbox
                .queue_observation()
                .await
                .expect("foundation-only outbox")
                .unresolved_failed,
            2
        );
        assert_eq!(
            provider
                .queue_observation()
                .await
                .expect("foundation-only provider")
                .unresolved_failed,
            2
        );
        fixture
            .execute(
                "UPDATE notification_outbox_lineage SET settled_at_ms = 2",
                (),
            )
            .await;
        let mut tx = fixture.uow.begin().await.expect("settlement transaction");
        EffectDescendantRepository::settle_all(&mut tx, descendant, chrono::Utc::now())
            .await
            .expect("settle descendant");
        tx.commit().await.expect("settlement commit");
        fixture.execute("UPDATE push_publish_jobs SET uncertain_send = 0, terminal_disposition = 'registration-revoked' WHERE job_id = 'parked'", ()).await;
        fixture.execute("UPDATE notification_outbox SET queue_acceptance_may_exist = 0 WHERE job_id = 'parked'", ()).await;
        assert_eq!(
            outbox.queue_observation().await.expect("settled outbox"),
            PushOutboxQueueObservation {
                parked: 0,
                unresolved_failed: 0,
            }
        );
        let retained = provider.queue_observation().await.expect("terminal tails");
        assert_eq!(retained.parked, 0);
        assert_eq!(retained.unresolved_failed, 0);
        assert_eq!(retained.retained_max_per_node, 9000);
        fixture.execute("DELETE FROM push_publish_jobs", ()).await;
        fixture.execute("DELETE FROM notification_outbox", ()).await;
        assert_eq!(
            provider
                .queue_observation()
                .await
                .expect("cleared provider"),
            PushProviderQueueObservation {
                parked: 0,
                unresolved_failed: 0,
                retained_max_per_node: 0,
                near_capacity_nodes: 0,
                local_registrations: None,
            }
        );
        assert_eq!(
            outbox.queue_observation().await.expect("cleared outbox"),
            PushOutboxQueueObservation {
                parked: 0,
                unresolved_failed: 0,
            }
        );
        fixture.close().await;
    }

    #[tokio::test]
    async fn sqlite_push_queue_observation_tracks_retained_capacity_and_unsettled_lineage() {
        push_queue_observation_tracks_retained_capacity_and_unsettled_lineage(
            IngressFixture::sqlite().await,
        )
        .await;
    }

    #[tokio::test]
    async fn postgres_push_queue_observation_tracks_retained_capacity_and_unsettled_lineage() {
        if let Some(fixture) = IngressFixture::postgres("push_queue_observation").await {
            push_queue_observation_tracks_retained_capacity_and_unsettled_lineage(fixture).await;
        }
    }

    #[tokio::test]
    async fn standalone_provider_queue_observation_without_ancestry_tables() {
        let store = crate::push_service::test_support::store().await;
        assert_eq!(
            store
                .queue_observation()
                .await
                .expect("standalone provider"),
            PushProviderQueueObservation {
                parked: 0,
                unresolved_failed: 0,
                retained_max_per_node: 0,
                near_capacity_nodes: 0,
                local_registrations: None,
            }
        );
    }
}
