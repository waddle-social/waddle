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

/// Validated LiveKit webhook delivery identifier — the idempotency
/// ledger's primary key. Classified once where the verified payload is
/// parsed so no raw string crosses the storage/CAS boundary (#1612
/// review round 9).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct WebhookEventId(String);

impl WebhookEventId {
    /// `None` for an empty/whitespace id: LiveKit has historically
    /// shipped payloads without one, and the dedupe path treats those
    /// as "not deduplicable" rather than keying the ledger on "".
    pub(crate) fn try_new(id: &str) -> Option<Self> {
        let id = id.trim();
        (!id.is_empty()).then(|| Self(id.to_owned()))
    }

    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for WebhookEventId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

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
    #[error("the restricted runtime must attach the owner-prepared webhook delivery ledger")]
    OwnerPreparationRequired,
    #[cfg(test)]
    #[error("in-memory webhook delivery store lock was poisoned")]
    LockPoisoned,
}

#[async_trait]
pub(crate) trait WebhookDeliveryStore: Send + Sync {
    async fn observe(
        &self,
        event_id: &WebhookEventId,
    ) -> Result<WebhookDeliveryObservation, WebhookDeliveryError>;

    async fn complete(&self, event_id: &WebhookEventId) -> Result<(), WebhookDeliveryError>;
}

pub(crate) struct DatabaseWebhookDeliveryStore {
    db: Database,
    initialized: OnceCell<()>,
    last_pruned_at_ms: std::sync::atomic::AtomicI64,
}

/// Failure to attach the delivery ledger under the restricted runtime role.
#[derive(Debug, Error)]
pub enum WebhookDeliveryAttachError {
    #[error("webhook delivery attachment requires the attested PostgreSQL main lane")]
    AttestedMainLaneRequired,
    #[error("the owner phase has not prepared the webhook delivery ledger")]
    OwnerPreparationMissing(#[source] DatabaseError),
    #[error("the webhook delivery ledger does not match the owner-prepared shape at {component}")]
    InvalidShape { component: &'static str },
    #[error("webhook delivery runtime role or privileges are unsafe at {component}")]
    UnsafeRuntimePrivileges { component: &'static str },
    #[error("webhook delivery attachment inspection timed out")]
    InspectionTimeout,
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

const ATTACH_INSPECTION_WAIT: std::time::Duration = std::time::Duration::from_secs(60);
const ATTACH_ROLLBACK_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// `(column, type, not null)` of the owner-prepared PostgreSQL ledger.
const POSTGRES_COLUMNS: [(&str, &str, bool); 5] = [
    ("event_id", "text", true),
    ("status", "text", true),
    ("attempt_count", "integer", true),
    ("first_seen_ms", "bigint", true),
    ("completed_at_ms", "bigint", false),
];

/// Create the delivery ledger and its retention index. The statements need
/// schema-owner privilege, so the restricted runtime role never runs them:
/// its owner phase calls this before the runtime attaches.
pub(crate) async fn initialize_owner_schema(db: &Database) -> Result<(), DatabaseError> {
    let timestamp_type = crate::db::i64_sql_type(db.driver());
    let connection = db.guard().await?;
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
    Ok(())
}

impl DatabaseWebhookDeliveryStore {
    pub(crate) fn new(db: Database) -> Self {
        Self {
            db,
            initialized: OnceCell::new(),
            last_pruned_at_ms: std::sync::atomic::AtomicI64::new(0),
        }
    }

    /// Attach to the owner-prepared ledger through the exact attested main
    /// lane. The check is read-only: a missing or drifted relation is a
    /// startup failure, never something the runtime role repairs.
    pub(crate) async fn attach_existing(db: Database) -> Result<Self, WebhookDeliveryAttachError> {
        verify_owner_prepared(&db).await?;
        Ok(Self {
            db,
            initialized: OnceCell::new_with(Some(())),
            last_pruned_at_ms: std::sync::atomic::AtomicI64::new(0),
        })
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
                // An attested pool belongs to the restricted runtime role,
                // which holds no DDL privilege. Only `attach_existing` may
                // mark such a store usable.
                if self.db.uses_attested_postgres_pool() {
                    return Err(WebhookDeliveryError::OwnerPreparationRequired);
                }
                initialize_owner_schema(&self.db)
                    .await
                    .map_err(WebhookDeliveryError::from)
            })
            .await?;
        Ok(())
    }

    async fn observe_at(
        &self,
        event_id: &WebhookEventId,
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
                crate::db_params![event_id.as_str(), now_ms],
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
                crate::db_params![event_id.as_str()],
            )
            .await?;
        if retried == 1 {
            transaction.commit().await?;
            return Ok(WebhookDeliveryObservation::Processing);
        }

        let mut rows = transaction
            .query(
                "SELECT status FROM webhook_deliveries WHERE event_id = ?",
                crate::db_params![event_id.as_str()],
            )
            .await?;
        let status = rows
            .next()
            .await?
            .ok_or_else(|| WebhookDeliveryError::UnknownDelivery(event_id.as_str().to_owned()))?
            .get::<String>(0)?;
        let observation = match status.as_str() {
            "done" => WebhookDeliveryObservation::Done,
            "processing" => WebhookDeliveryObservation::Processing,
            _ => return Err(WebhookDeliveryError::InvalidStatus(status)),
        };
        transaction.commit().await?;
        Ok(observation)
    }

    async fn complete_at(
        &self,
        event_id: &WebhookEventId,
        now_ms: i64,
    ) -> Result<(), WebhookDeliveryError> {
        self.initialize().await?;
        let connection = self.db.guard().await?;
        let updated = connection
            .execute(
                "UPDATE webhook_deliveries \
                 SET status = 'done', completed_at_ms = ? \
                 WHERE event_id = ? AND status = 'processing'",
                crate::db_params![now_ms, event_id.as_str()],
            )
            .await?;
        if updated == 1 {
            return Ok(());
        }

        let mut rows = connection
            .query(
                "SELECT status FROM webhook_deliveries WHERE event_id = ?",
                crate::db_params![event_id.as_str()],
            )
            .await?;
        let status = rows
            .next()
            .await?
            .ok_or_else(|| WebhookDeliveryError::UnknownDelivery(event_id.as_str().to_owned()))?
            .get::<String>(0)?;
        match status.as_str() {
            "done" => Ok(()),
            "processing" => Err(WebhookDeliveryError::UnknownDelivery(
                event_id.as_str().to_owned(),
            )),
            _ => Err(WebhookDeliveryError::InvalidStatus(status)),
        }
    }

    #[cfg(test)]
    async fn attempt_count(&self, event_id: &WebhookEventId) -> Result<i64, WebhookDeliveryError> {
        self.initialize().await?;
        let connection = self.db.guard().await?;
        let mut rows = connection
            .query(
                "SELECT attempt_count FROM webhook_deliveries WHERE event_id = ?",
                crate::db_params![event_id.as_str()],
            )
            .await?;
        rows.next()
            .await?
            .ok_or_else(|| WebhookDeliveryError::UnknownDelivery(event_id.as_str().to_owned()))?
            .get::<i64>(0)
            .map_err(WebhookDeliveryError::from)
    }
}

/// Verify the owner-prepared ledger in one bounded read-only transaction on
/// the attested main lane: relation shape, primary key, status constraint,
/// retention index, and the exact runtime table privileges.
async fn verify_owner_prepared(db: &Database) -> Result<(), WebhookDeliveryAttachError> {
    if db.driver() != crate::db::DatabaseDriver::Postgres || !db.is_main_lane() {
        return Err(WebhookDeliveryAttachError::AttestedMainLaneRequired);
    }
    let capability = db
        .attested_pg_writer_capability()
        .ok_or(WebhookDeliveryAttachError::AttestedMainLaneRequired)?;
    db.verify_attested_pg_writer_capability(&capability)
        .map_err(DatabaseError::from)?;
    let schema = capability.identity().schema.name.0.clone();
    let mut tx = db.begin_catalog_fenced_read_only(&[]).await?;
    let inspect = async {
        crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
            .await
            .map_err(|error| match error {
                crate::db::read_only_timeout::ReadOnlyTimeoutError::Database(error) => {
                    WebhookDeliveryAttachError::Database(error)
                }
                crate::db::read_only_timeout::ReadOnlyTimeoutError::InvalidSetting { .. } => {
                    WebhookDeliveryAttachError::InvalidShape {
                        component: "transaction wait bounds",
                    }
                }
            })?;
        let quoted = crate::db::physical_attestation::quote_identifier(&schema);
        tx.execute(
            &format!("LOCK TABLE {quoted}.\"webhook_deliveries\" IN ACCESS SHARE MODE"),
            (),
        )
        .await
        .map_err(WebhookDeliveryAttachError::OwnerPreparationMissing)?;
        capability.attest_transaction(&mut tx).await?;
        verify_shape(&mut tx, &schema).await?;
        if let Some(component) =
            crate::db::runtime_store_acl::unexpected_webhook_executable_schema(&mut tx, &schema)
                .await?
        {
            return Err(WebhookDeliveryAttachError::InvalidShape { component });
        }
        if let Some(component) =
            crate::db::runtime_store_acl::verify_webhook_deliveries(&mut tx, &schema).await?
        {
            return Err(WebhookDeliveryAttachError::UnsafeRuntimePrivileges { component });
        }
        Ok::<(), WebhookDeliveryAttachError>(())
    };
    let inspection = tokio::time::timeout(ATTACH_INSPECTION_WAIT, inspect)
        .await
        .unwrap_or(Err(WebhookDeliveryAttachError::InspectionTimeout));
    if !tx.rollback_bounded_discard(ATTACH_ROLLBACK_WAIT).await {
        return Err(DatabaseError::CatalogMutationRollbackUnconfirmed.into());
    }
    inspection
}

async fn verify_shape(
    tx: &mut crate::db::Transaction<'_>,
    schema: &str,
) -> Result<(), WebhookDeliveryAttachError> {
    let mut rows = tx
        .query(
            "SELECT a.attname::text, pg_catalog.format_type(a.atttypid, a.atttypmod), a.attnotnull \
             FROM pg_catalog.pg_attribute a \
             JOIN pg_catalog.pg_class c ON c.oid = a.attrelid \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = 'webhook_deliveries' \
               AND c.relkind = 'r' AND c.relpersistence = 'p' \
               AND a.attnum > 0 AND NOT a.attisdropped \
             ORDER BY a.attnum",
            crate::db_params![schema],
        )
        .await?;
    let mut columns = Vec::new();
    while let Some(row) = rows.next().await? {
        columns.push((
            row.get::<String>(0)?,
            row.get::<String>(1)?,
            row.get::<bool>(2)?,
        ));
    }
    drop(rows);
    let matches = columns.len() == POSTGRES_COLUMNS.len()
        && columns.iter().zip(POSTGRES_COLUMNS).all(
            |((name, kind, not_null), (expected_name, expected_kind, expected_not_null))| {
                name == expected_name && kind == expected_kind && *not_null == expected_not_null
            },
        );
    if !matches {
        return Err(WebhookDeliveryAttachError::InvalidShape {
            component: "columns",
        });
    }
    let mut rows = tx
        .query(
            "SELECT \
               (SELECT pg_catalog.count(*) FROM pg_catalog.pg_constraint k \
                 WHERE k.conrelid = c.oid AND k.contype = 'p' AND k.convalidated \
                   AND ARRAY(SELECT a.attname::text \
                     FROM pg_catalog.unnest(k.conkey) WITH ORDINALITY key(attnum, position) \
                     JOIN pg_catalog.pg_attribute a \
                       ON a.attrelid = c.oid AND a.attnum = key.attnum \
                     ORDER BY key.position) = ARRAY['event_id']) = 1, \
               (SELECT pg_catalog.count(*) FROM pg_catalog.pg_constraint k \
                 WHERE k.conrelid = c.oid AND k.contype = 'c' AND k.convalidated \
                   AND pg_catalog.pg_get_constraintdef(k.oid) = \
                     'CHECK ((status = ANY (ARRAY[''processing''::text, ''done''::text])))') = 1, \
               (SELECT pg_catalog.count(*) FROM pg_catalog.pg_index x \
                 JOIN pg_catalog.pg_class i ON i.oid = x.indexrelid \
                 WHERE x.indrelid = c.oid AND i.relname = 'idx_webhook_deliveries_done_prune' \
                   AND x.indisvalid AND x.indisready AND x.indislive AND NOT x.indisunique \
                   AND x.indpred IS NULL AND x.indexprs IS NULL \
                   AND ARRAY(SELECT a.attname::text \
                     FROM pg_catalog.unnest(x.indkey) WITH ORDINALITY key(attnum, position) \
                     JOIN pg_catalog.pg_attribute a \
                       ON a.attrelid = c.oid AND a.attnum = key.attnum \
                     ORDER BY key.position) = ARRAY['status', 'completed_at_ms']) = 1 \
             FROM pg_catalog.pg_class c \
             JOIN pg_catalog.pg_namespace n ON n.oid = c.relnamespace \
             WHERE n.nspname = ? AND c.relname = 'webhook_deliveries'",
            crate::db_params![schema],
        )
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or(WebhookDeliveryAttachError::InvalidShape {
            component: "relation",
        })?;
    for (index, component) in ["primary key", "status constraint", "retention index"]
        .into_iter()
        .enumerate()
    {
        if !row.get::<bool>(index)? {
            return Err(WebhookDeliveryAttachError::InvalidShape { component });
        }
    }
    Ok(())
}

#[async_trait]
impl WebhookDeliveryStore for DatabaseWebhookDeliveryStore {
    async fn observe(
        &self,
        event_id: &WebhookEventId,
    ) -> Result<WebhookDeliveryObservation, WebhookDeliveryError> {
        self.observe_at(event_id, crate::time::now_ms()).await
    }

    async fn complete(&self, event_id: &WebhookEventId) -> Result<(), WebhookDeliveryError> {
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
        event_id: &WebhookEventId,
    ) -> Result<Option<WebhookDeliveryObservation>, WebhookDeliveryError> {
        let deliveries = self
            .deliveries
            .lock()
            .map_err(|_| WebhookDeliveryError::LockPoisoned)?;
        Ok(deliveries
            .get(event_id.as_str())
            .map(|delivery| delivery.observation))
    }

    pub(crate) fn attempt_count(
        &self,
        event_id: &WebhookEventId,
    ) -> Result<Option<u64>, WebhookDeliveryError> {
        let deliveries = self
            .deliveries
            .lock()
            .map_err(|_| WebhookDeliveryError::LockPoisoned)?;
        Ok(deliveries
            .get(event_id.as_str())
            .map(|delivery| delivery.attempt_count))
    }
}

#[cfg(test)]
#[async_trait]
impl WebhookDeliveryStore for InMemoryWebhookDeliveryStore {
    async fn observe(
        &self,
        event_id: &WebhookEventId,
    ) -> Result<WebhookDeliveryObservation, WebhookDeliveryError> {
        let mut deliveries = self
            .deliveries
            .lock()
            .map_err(|_| WebhookDeliveryError::LockPoisoned)?;
        let delivery = deliveries
            .entry(event_id.as_str().to_owned())
            .or_insert(InMemoryDelivery {
                observation: WebhookDeliveryObservation::Processing,
                attempt_count: 0,
            });
        if delivery.observation == WebhookDeliveryObservation::Processing {
            delivery.attempt_count = delivery.attempt_count.saturating_add(1);
        }
        Ok(delivery.observation)
    }

    async fn complete(&self, event_id: &WebhookEventId) -> Result<(), WebhookDeliveryError> {
        let mut deliveries = self
            .deliveries
            .lock()
            .map_err(|_| WebhookDeliveryError::LockPoisoned)?;
        let delivery = deliveries
            .get_mut(event_id.as_str())
            .ok_or_else(|| WebhookDeliveryError::UnknownDelivery(event_id.as_str().to_owned()))?;
        delivery.observation = WebhookDeliveryObservation::Done;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ev(id: &str) -> WebhookEventId {
        WebhookEventId::try_new(id).expect("valid test event id")
    }

    #[tokio::test]
    async fn processing_delivery_is_retried_and_attempt_count_increments() {
        let db = Database::in_memory("webhook-delivery-processing")
            .await
            .expect("in-memory database");
        let store = DatabaseWebhookDeliveryStore::new(db);

        assert_eq!(
            store
                .observe(&ev("EV_processing"))
                .await
                .expect("first observe"),
            WebhookDeliveryObservation::Processing
        );
        assert_eq!(
            store
                .observe(&ev("EV_processing"))
                .await
                .expect("retry observe"),
            WebhookDeliveryObservation::Processing
        );
        assert_eq!(
            store
                .attempt_count(&ev("EV_processing"))
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
            first_store
                .observe(&ev("EV_restart"))
                .await
                .expect("observe"),
            WebhookDeliveryObservation::Processing
        );
        first_store
            .complete(&ev("EV_restart"))
            .await
            .expect("complete");

        let restarted_store = DatabaseWebhookDeliveryStore::new(db);
        assert_eq!(
            restarted_store
                .observe(&ev("EV_restart"))
                .await
                .expect("observe after restart"),
            WebhookDeliveryObservation::Done
        );
        assert_eq!(
            restarted_store
                .attempt_count(&ev("EV_restart"))
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
        store
            .observe_at(&ev("EV_old"), 0)
            .await
            .expect("observe old");
        store
            .complete_at(&ev("EV_old"), 1)
            .await
            .expect("complete old");
        store
            .observe_at(&ev("EV_processing"), DONE_RETENTION_MS + 1)
            .await
            .expect("observe processing");

        // Pruning is amortized to one observe per PRUNE_INTERVAL_MS, so
        // the triggering observe must land in a fresh interval slot.
        store
            .observe_at(&ev("EV_trigger"), DONE_RETENTION_MS + PRUNE_INTERVAL_MS + 2)
            .await
            .expect("trigger prune");

        assert_eq!(
            store
                .observe_at(&ev("EV_old"), DONE_RETENTION_MS + PRUNE_INTERVAL_MS + 3)
                .await
                .expect("re-observe expired delivery"),
            WebhookDeliveryObservation::Processing,
            "expired done row was deleted and can be inserted fresh"
        );
        assert_eq!(
            store
                .attempt_count(&ev("EV_processing"))
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
            .observe_at(&ev("EV_stuck_processing"), 0)
            .await
            .expect("seed processing row");

        store
            .observe_at(&ev("EV_trigger"), PROCESSING_RETENTION_MS + 1)
            .await
            .expect("trigger prune");

        assert_eq!(
            store
                .observe_at(&ev("EV_stuck_processing"), PROCESSING_RETENTION_MS + 2)
                .await
                .expect("re-observe pruned processing row"),
            WebhookDeliveryObservation::Processing,
            "stale processing row was deleted and can be inserted fresh"
        );
        assert_eq!(
            store
                .attempt_count(&ev("EV_stuck_processing"))
                .await
                .expect("attempt count after prune"),
            1
        );
    }
}
