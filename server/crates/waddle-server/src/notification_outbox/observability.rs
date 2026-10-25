//! A current inventory, independent of retry activity or historical counters.

use super::{NotificationOutboxError, NotificationOutboxStore};
use crate::db::DatabaseError;
use waddle_xmpp::telemetry::attributes::PushQueue;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushOutboxQueueObservation {
    pub parked: u64,
    pub unresolved_failed: u64,
}

impl PushOutboxQueueObservation {
    pub(crate) fn record(self) {
        waddle_xmpp::telemetry::reliability::record_push_queue_snapshot(
            PushQueue::Outbox,
            self.parked,
            self.unresolved_failed,
            crate::time::now_ms().max(0) as u64 / 1_000,
        );
    }
}

impl NotificationOutboxStore {
    pub async fn queue_observation(
        &self,
    ) -> Result<PushOutboxQueueObservation, NotificationOutboxError> {
        let (_, foundation) = crate::push_service::observability::ancestry_tables(&self.db).await?;
        let foundation_guard = if foundation {
            "EXISTS(SELECT 1 FROM ingress_effect_descendants d WHERE d.descendant_key = o.job_id AND d.settled_at IS NULL)"
        } else {
            "FALSE"
        };
        let sql = format!(
            "SELECT COALESCE(SUM(CASE WHEN status = 'failed' AND queue_acceptance_may_exist = 1 THEN 1 ELSE 0 END), 0),
                COALESCE(SUM(CASE WHEN status = 'failed' AND queue_acceptance_may_exist = 0 AND (
                    EXISTS(SELECT 1 FROM notification_outbox_lineage l WHERE l.job_id = o.job_id AND l.settled_at_ms IS NULL)
                    OR {foundation_guard}) THEN 1 ELSE 0 END), 0)
             FROM notification_outbox o"
        );
        let mut rows = self.query(&sql, ()).await?;
        let row = rows
            .next()
            .await?
            .ok_or_else(|| DatabaseError::QueryFailed("push outbox aggregate missing".into()))?;
        let parked: i64 = row.get(0)?;
        let unresolved_failed: i64 = row.get(1)?;
        Ok(PushOutboxQueueObservation {
            parked: parked.max(0) as u64,
            unresolved_failed: unresolved_failed.max(0) as u64,
        })
    }
}
