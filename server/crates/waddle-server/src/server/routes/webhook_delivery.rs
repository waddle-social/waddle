//! Durable delivery ledger for LiveKit webhook idempotency.

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::Mutex;

use async_trait::async_trait;
use thiserror::Error;
use tokio::sync::OnceCell;

use crate::db::{Database, DatabaseError};

const DONE_RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;
const PROCESSING_RETENTION_MS: i64 = 24 * 60 * 60 * 1000;
const PRUNE_BATCH_SIZE: i64 = 128;
/// Retention pruning is amortized: at most one observe per interval pays
/// the DELETE cost, keeping the hot webhook-ingest transaction small.
const PRUNE_INTERVAL_MS: i64 = 60 * 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebhookDeliveryObservation {
    Processing,
    Done,
}

#[derive(Debug, Error)]
pub(crate) enum WebhookDeliveryError {
    #[error(transparent)]
    Database(#[from] DatabaseError),
    #[error("webhook delivery row has invalid status '{0}'")]
    InvalidStatus(String),
    #[error("webhook delivery '{0}' was not observed before completion")]
    UnknownDelivery(String),
    #[cfg(test)]
    #[error("in-memory webhook delivery store lock was poisoned")]
    LockPoisoned,
}

#[async_trait]
pub(crate) trait WebhookDeliveryStore: Send + Sync {
    async fn observe(
        &self,
        event_id: &str,
    ) -> Result<WebhookDeliveryObservation, WebhookDeliveryError>;

    async fn complete(&self, event_id: &str) -> Result<(), WebhookDeliveryError>;
}

pub(crate) struct DatabaseWebhookDeliveryStore {
    db: Database,
    initialized: OnceCell<()>,
    last_pruned_at_ms: std::sync::atomic::AtomicI64,
}

impl DatabaseWebhookDeliveryStore {
    pub(crate) fn new(db: Database) -> Self {
        Self {
            db,
            initialized: OnceCell::new(),
            last_pruned_at_ms: std::sync::atomic::AtomicI64::new(0),
        }
    }

    /// At most one observe per [`PRUNE_INTERVAL_MS`] wins the CAS and
    /// carries the retention DELETEs; everyone else skips them. The
    /// first observe of a store's lifetime (sentinel `0`) always
    /// prunes, independent of the caller's clock domain.
    fn claim_prune_slot(&self, now_ms: i64) -> bool {
        let last = self
            .last_pruned_at_ms
            .load(std::sync::atomic::Ordering::Relaxed);
        (last == 0 || now_ms.saturating_sub(last) >= PRUNE_INTERVAL_MS)
            && self
                .last_pruned_at_ms
                .compare_exchange(
                    last,
                    now_ms.max(1),
                    std::sync::atomic::Ordering::Relaxed,
                    std::sync::atomic::Ordering::Relaxed,
                )
                .is_ok()
    }

    async fn initialize(&self) -> Result<(), WebhookDeliveryError> {
        self.initialized
            .get_or_try_init(|| async {
                let timestamp_type = crate::db::i64_sql_type(self.db.driver());
                let connection = self.db.guard().await?;
                connection
                    .execute(
                        &format!(
                            "CREATE TABLE IF NOT EXISTS webhook_deliveries (\
                                event_id TEXT PRIMARY KEY, \
                                status TEXT NOT NULL CHECK (status IN ('processing','done')), \
                                attempt_count INTEGER NOT NULL, \
                                first_seen_ms {timestamp_type} NOT NULL, \
                                completed_at_ms {timestamp_type} NULL\
                            )"
                        ),
                        (),
                    )
                    .await?;
                connection
                    .execute(
                        "CREATE INDEX IF NOT EXISTS idx_webhook_deliveries_done_prune \
                         ON webhook_deliveries (status, completed_at_ms)",
                        (),
                    )
                    .await?;
                Ok::<(), DatabaseError>(())
            })
            .await?;
        Ok(())
    }

    async fn observe_at(
        &self,
        event_id: &str,
        now_ms: i64,
    ) -> Result<WebhookDeliveryObservation, WebhookDeliveryError> {
        self.initialize().await?;
        let mut transaction = self.db.begin_immediate().await?;
        if self.claim_prune_slot(now_ms) {
            let prune_before_ms = now_ms.saturating_sub(DONE_RETENTION_MS);
            let processing_prune_before_ms = now_ms.saturating_sub(PROCESSING_RETENTION_MS);
            transaction
                .execute(
                    "DELETE FROM webhook_deliveries \
                     WHERE event_id IN (\
                         SELECT event_id FROM webhook_deliveries \
                         WHERE status = 'done' AND completed_at_ms < ? \
                         ORDER BY completed_at_ms ASC LIMIT ?\
                     )",
                    crate::db_params![prune_before_ms, PRUNE_BATCH_SIZE],
                )
                .await?;
            // LiveKit abandons webhook retries within minutes. Reaping a
            // day-old `processing` row therefore only re-opens idempotent
            // reprocessing for a delivery we will not hear about again.
            transaction
                .execute(
                    "DELETE FROM webhook_deliveries \
                     WHERE event_id IN (\
                         SELECT event_id FROM webhook_deliveries \
                         WHERE status = 'processing' AND first_seen_ms < ? \
                         ORDER BY first_seen_ms ASC LIMIT ?\
                     )",
                    crate::db_params![processing_prune_before_ms, PRUNE_BATCH_SIZE],
                )
                .await?;
        }

        let inserted = transaction
            .execute(
                "INSERT INTO webhook_deliveries (\
                    event_id, status, attempt_count, first_seen_ms, completed_at_ms\
                 ) VALUES (?, 'processing', 1, ?, NULL) \
                 ON CONFLICT(event_id) DO NOTHING",
                crate::db_params![event_id, now_ms],
            )
            .await?;
        if inserted == 1 {
            transaction.commit().await?;
            return Ok(WebhookDeliveryObservation::Processing);
        }

        let retried = transaction
            .execute(
                "UPDATE webhook_deliveries \
                 SET attempt_count = attempt_count + 1 \
                 WHERE event_id = ? AND status = 'processing'",
                crate::db_params![event_id],
            )
            .await?;
        if retried == 1 {
            transaction.commit().await?;
            return Ok(WebhookDeliveryObservation::Processing);
        }

        let mut rows = transaction
            .query(
                "SELECT status FROM webhook_deliveries WHERE event_id = ?",
                crate::db_params![event_id],
            )
            .await?;
        let status = rows
            .next()
            .await?
            .ok_or_else(|| WebhookDeliveryError::UnknownDelivery(event_id.to_owned()))?
            .get::<String>(0)?;
        let observation = match status.as_str() {
            "done" => WebhookDeliveryObservation::Done,
            "processing" => WebhookDeliveryObservation::Processing,
            _ => return Err(WebhookDeliveryError::InvalidStatus(status)),
        };
        transaction.commit().await?;
        Ok(observation)
    }

    async fn complete_at(&self, event_id: &str, now_ms: i64) -> Result<(), WebhookDeliveryError> {
        self.initialize().await?;
        let connection = self.db.guard().await?;
        let updated = connection
            .execute(
                "UPDATE webhook_deliveries \
                 SET status = 'done', completed_at_ms = ? \
                 WHERE event_id = ? AND status = 'processing'",
                crate::db_params![now_ms, event_id],
            )
            .await?;
        if updated == 1 {
            return Ok(());
        }

        let mut rows = connection
            .query(
                "SELECT status FROM webhook_deliveries WHERE event_id = ?",
                crate::db_params![event_id],
            )
            .await?;
        let status = rows
            .next()
            .await?
            .ok_or_else(|| WebhookDeliveryError::UnknownDelivery(event_id.to_owned()))?
            .get::<String>(0)?;
        match status.as_str() {
            "done" => Ok(()),
            "processing" => Err(WebhookDeliveryError::UnknownDelivery(event_id.to_owned())),
            _ => Err(WebhookDeliveryError::InvalidStatus(status)),
        }
    }

    #[cfg(test)]
    async fn attempt_count(&self, event_id: &str) -> Result<i64, WebhookDeliveryError> {
        self.initialize().await?;
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                "SELECT attempt_count FROM webhook_deliveries WHERE event_id = ?",
                crate::db_params![event_id],
            )
            .await?;
        rows.next()
            .await?
            .ok_or_else(|| WebhookDeliveryError::UnknownDelivery(event_id.to_owned()))?
            .get::<i64>(0)
            .map_err(WebhookDeliveryError::from)
    }
}

#[async_trait]
impl WebhookDeliveryStore for DatabaseWebhookDeliveryStore {
    async fn observe(
        &self,
        event_id: &str,
    ) -> Result<WebhookDeliveryObservation, WebhookDeliveryError> {
        self.observe_at(event_id, crate::time::now_ms()).await
    }

    async fn complete(&self, event_id: &str) -> Result<(), WebhookDeliveryError> {
        self.complete_at(event_id, crate::time::now_ms()).await
    }
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(crate) struct InMemoryWebhookDeliveryStore {
    deliveries: Mutex<HashMap<String, InMemoryDelivery>>,
}

#[cfg(test)]
#[derive(Debug, Clone, Copy)]
struct InMemoryDelivery {
    observation: WebhookDeliveryObservation,
    attempt_count: u64,
}

#[cfg(test)]
impl InMemoryWebhookDeliveryStore {
    pub(crate) fn status(
        &self,
        event_id: &str,
    ) -> Result<Option<WebhookDeliveryObservation>, WebhookDeliveryError> {
        let deliveries = self
            .deliveries
            .lock()
            .map_err(|_| WebhookDeliveryError::LockPoisoned)?;
        Ok(deliveries
            .get(event_id)
            .map(|delivery| delivery.observation))
    }

    pub(crate) fn attempt_count(
        &self,
        event_id: &str,
    ) -> Result<Option<u64>, WebhookDeliveryError> {
        let deliveries = self
            .deliveries
            .lock()
            .map_err(|_| WebhookDeliveryError::LockPoisoned)?;
        Ok(deliveries
            .get(event_id)
            .map(|delivery| delivery.attempt_count))
    }
}

#[cfg(test)]
#[async_trait]
impl WebhookDeliveryStore for InMemoryWebhookDeliveryStore {
    async fn observe(
        &self,
        event_id: &str,
    ) -> Result<WebhookDeliveryObservation, WebhookDeliveryError> {
        let mut deliveries = self
            .deliveries
            .lock()
            .map_err(|_| WebhookDeliveryError::LockPoisoned)?;
        let delivery = deliveries
            .entry(event_id.to_owned())
            .or_insert(InMemoryDelivery {
                observation: WebhookDeliveryObservation::Processing,
                attempt_count: 0,
            });
        if delivery.observation == WebhookDeliveryObservation::Processing {
            delivery.attempt_count = delivery.attempt_count.saturating_add(1);
        }
        Ok(delivery.observation)
    }

    async fn complete(&self, event_id: &str) -> Result<(), WebhookDeliveryError> {
        let mut deliveries = self
            .deliveries
            .lock()
            .map_err(|_| WebhookDeliveryError::LockPoisoned)?;
        let delivery = deliveries
            .get_mut(event_id)
            .ok_or_else(|| WebhookDeliveryError::UnknownDelivery(event_id.to_owned()))?;
        delivery.observation = WebhookDeliveryObservation::Done;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn processing_delivery_is_retried_and_attempt_count_increments() {
        let db = Database::in_memory("webhook-delivery-processing")
            .await
            .expect("in-memory database");
        let store = DatabaseWebhookDeliveryStore::new(db);

        assert_eq!(
            store.observe("EV_processing").await.expect("first observe"),
            WebhookDeliveryObservation::Processing
        );
        assert_eq!(
            store.observe("EV_processing").await.expect("retry observe"),
            WebhookDeliveryObservation::Processing
        );
        assert_eq!(
            store
                .attempt_count("EV_processing")
                .await
                .expect("attempt count"),
            2
        );
    }

    #[tokio::test]
    async fn completed_delivery_survives_store_reconstruction() {
        let db = Database::in_memory("webhook-delivery-restart")
            .await
            .expect("in-memory database");
        let first_store = DatabaseWebhookDeliveryStore::new(db.clone());
        assert_eq!(
            first_store.observe("EV_restart").await.expect("observe"),
            WebhookDeliveryObservation::Processing
        );
        first_store.complete("EV_restart").await.expect("complete");

        let restarted_store = DatabaseWebhookDeliveryStore::new(db);
        assert_eq!(
            restarted_store
                .observe("EV_restart")
                .await
                .expect("observe after restart"),
            WebhookDeliveryObservation::Done
        );
        assert_eq!(
            restarted_store
                .attempt_count("EV_restart")
                .await
                .expect("attempt count"),
            1,
            "done duplicates do not increment attempts"
        );
    }

    #[tokio::test]
    async fn observe_prunes_only_completed_rows_older_than_retention() {
        let db = Database::in_memory("webhook-delivery-prune")
            .await
            .expect("in-memory database");
        let store = DatabaseWebhookDeliveryStore::new(db);
        store.observe_at("EV_old", 0).await.expect("observe old");
        store.complete_at("EV_old", 1).await.expect("complete old");
        store
            .observe_at("EV_processing", DONE_RETENTION_MS + 1)
            .await
            .expect("observe processing");

        // Pruning is amortized to one observe per PRUNE_INTERVAL_MS, so
        // the triggering observe must land in a fresh interval slot.
        store
            .observe_at("EV_trigger", DONE_RETENTION_MS + PRUNE_INTERVAL_MS + 2)
            .await
            .expect("trigger prune");

        assert_eq!(
            store
                .observe_at("EV_old", DONE_RETENTION_MS + PRUNE_INTERVAL_MS + 3)
                .await
                .expect("re-observe expired delivery"),
            WebhookDeliveryObservation::Processing,
            "expired done row was deleted and can be inserted fresh"
        );
        assert_eq!(
            store
                .attempt_count("EV_processing")
                .await
                .expect("processing attempt count"),
            1,
            "processing rows are never pruned"
        );
    }

    #[tokio::test]
    async fn observe_prunes_processing_rows_older_than_a_day() {
        let db = Database::in_memory("webhook-delivery-processing-prune")
            .await
            .expect("in-memory database");
        let store = DatabaseWebhookDeliveryStore::new(db);
        store
            .observe_at("EV_stuck_processing", 0)
            .await
            .expect("seed processing row");

        store
            .observe_at("EV_trigger", PROCESSING_RETENTION_MS + 1)
            .await
            .expect("trigger prune");

        assert_eq!(
            store
                .observe_at("EV_stuck_processing", PROCESSING_RETENTION_MS + 2)
                .await
                .expect("re-observe pruned processing row"),
            WebhookDeliveryObservation::Processing,
            "stale processing row was deleted and can be inserted fresh"
        );
        assert_eq!(
            store
                .attempt_count("EV_stuck_processing")
                .await
                .expect("attempt count after prune"),
            1
        );
    }
}
