//! Dark, single-transaction custody inspection for the Push and notification
//! store family.
//!
//! This is diagnostic evidence only. It never returns store or signer
//! handles, changes rows, or certifies that serving may be enabled. The
//! ancestry inventory is one SQL statement so all mutable rows are classified
//! from the same PostgreSQL command snapshot.

use std::time::Duration;

use thiserror::Error;

use crate::db::{Database, DatabaseDriver, DatabaseError, Row, Transaction};
use crate::notification_activity::runtime_attach as runtime;
use crate::notification_activity::runtime_attach::{
    DurableStoreAttachError, ACTIVITY_CONTRACT, PUSH_REGISTRATIONS_CONTRACT,
};
use crate::notification_outbox::NotificationOutboxRuntimeAttachError;
use crate::pubsub::PubSubRuntimeAttachError;
use crate::push_service::vapid_storage::VapidRuntimeAttachError;
use crate::push_service::PushServiceAttachError;

const FOUNDATION_DESCENDANT_TABLE: &str = "ingress_effect_descendants";
const CUSTODY_GATE_WAIT: Duration = Duration::from_secs(15);
const CUSTODY_GATE_RETRY: Duration = Duration::from_millis(20);
const CUSTODY_ROLLBACK_WAIT: Duration = Duration::from_secs(5);

/// The result contains counts only. It is intentionally not a store handle or
/// a readiness certificate; callers cannot use it to serve Push traffic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PushNotificationCustodyEvidence {
    classified_outbox_jobs: u64,
    unmatched_legacy_outbox_obligations: u64,
    classified_open_candidates: u64,
    unmatched_legacy_candidates: u64,
    classified_push_obligations: u64,
    unmatched_legacy_push_obligations: u64,
    unmatched_wire_push_obligations: u64,
    classified_foundation_descendants: u64,
    unmatched_legacy_foundation_descendants: u64,
    active_outbox_jobs: u64,
    parked_uncertain_outbox_jobs: u64,
    active_push_jobs: u64,
    parked_uncertain_push_jobs: u64,
    pending_notification_lineage_rows: u64,
    pending_foundation_descendants: u64,
}

impl std::fmt::Display for PushNotificationCustodyEvidence {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "outbox[classified={}, unmatched_legacy={}, active={}, parked_uncertain={}]; \
             candidates[classified={}, unmatched_legacy={}]; \
             push[classified={}, unmatched_legacy={}, unmatched_wire={}, active={}, parked_uncertain={}]; \
             foundation[classified={}, unmatched_legacy={}, pending={}]; \
             lineage[pending={}]",
            self.classified_outbox_jobs,
            self.unmatched_legacy_outbox_obligations,
            self.active_outbox_jobs,
            self.parked_uncertain_outbox_jobs,
            self.classified_open_candidates,
            self.unmatched_legacy_candidates,
            self.classified_push_obligations,
            self.unmatched_legacy_push_obligations,
            self.unmatched_wire_push_obligations,
            self.active_push_jobs,
            self.parked_uncertain_push_jobs,
            self.classified_foundation_descendants,
            self.unmatched_legacy_foundation_descendants,
            self.pending_foundation_descendants,
            self.pending_notification_lineage_rows,
        )
    }
}

/// Failure while inspecting the Push and notification custody boundary.
#[derive(Debug, Error)]
pub enum PushNotificationCustodyError {
    #[error("Push and notification custody inspection requires PostgreSQL")]
    PostgresRequired,
    #[error("Push and notification custody inspection requires the main database lane")]
    MainLaneRequired,
    #[error("Push and notification custody inspection requires an attested main pool")]
    AttestedPoolRequired,
    #[error("the attested PostgreSQL capability is invalid")]
    InvalidPoolCapability(#[source] DatabaseError),
    #[error("the attested PostgreSQL schema identifier is invalid")]
    InvalidSchema,
    #[error("could not bound PostgreSQL waits for Push and notification custody inspection")]
    ReadBounds,
    #[error("Foundation relation {table} is absent or could not be locked")]
    FoundationRelation {
        table: &'static str,
        #[source]
        source: DatabaseError,
    },
    #[error("the custody snapshot returned an invalid row or count")]
    InvalidSnapshot,
    #[error("the custody snapshot contains {rows} contradictory rows")]
    ContradictoryRows { rows: u64 },
    #[error(transparent)]
    Outbox(#[from] NotificationOutboxRuntimeAttachError),
    #[error(transparent)]
    PushService(#[from] PushServiceAttachError),
    #[error(transparent)]
    Vapid(#[from] VapidRuntimeAttachError),
    #[error(transparent)]
    NotificationStore(#[from] DurableStoreAttachError),
    #[error(transparent)]
    PubSub(#[from] PubSubRuntimeAttachError),
    #[error(transparent)]
    Database(#[from] DatabaseError),
}

/// Inspect owner-prepared Push, notification, and PubSub storage through one
/// bounded read-only transaction on the exact attested main pool. Outbox
/// locks precede Push locks, matching the Push owner ancestry phase. Pending,
/// in-flight, and failed-uncertain work is retained as custody evidence; the
/// diagnostic does not require empty queues and does not mutate them.
pub async fn verify_attested_push_notification_custody(
    database: Database,
    root_key: &[u8],
) -> Result<PushNotificationCustodyEvidence, PushNotificationCustodyError> {
    if database.driver() != DatabaseDriver::Postgres {
        return Err(PushNotificationCustodyError::PostgresRequired);
    }
    if !database.is_main_lane() {
        return Err(PushNotificationCustodyError::MainLaneRequired);
    }
    let capability = database
        .attested_pg_writer_capability()
        .ok_or(PushNotificationCustodyError::AttestedPoolRequired)?;
    database
        .verify_attested_pg_writer_capability(&capability)
        .map_err(DatabaseError::from)
        .map_err(PushNotificationCustodyError::InvalidPoolCapability)?;
    let identity = capability.identity();
    let schema = identity.schema.name.0.as_str();
    crate::db::physical_attestation::validate_identifier(schema, "custody diagnostic schema")
        .map_err(|_| PushNotificationCustodyError::InvalidSchema)?;

    let deadline = tokio::time::Instant::now() + CUSTODY_GATE_WAIT;
    let mut tx = loop {
        let mut tx =
            tokio::time::timeout_at(deadline, database.begin_catalog_fenced_read_only(&[]))
                .await
                .map_err(|_| DatabaseError::CatalogMutationLockTimeout)??;
        let attempt = tokio::time::timeout_at(deadline, async {
            crate::db::read_only_timeout::bound_postgres_waits(&mut tx)
                .await
                .map_err(|error| match error {
                    crate::db::read_only_timeout::ReadOnlyTimeoutError::InvalidSetting {
                        ..
                    } => PushNotificationCustodyError::ReadBounds,
                    crate::db::read_only_timeout::ReadOnlyTimeoutError::Database(error) => {
                        PushNotificationCustodyError::Database(error)
                    }
                })?;

            // Each gate is tried in its original position, immediately before
            // that store's relation locks. A failed try restarts the entire
            // operation after catalog-S and earlier locks are released.
            if !crate::db::try_catalog_fence_lock(
                &mut tx,
                crate::notification_outbox::runtime_attach::CUSTODY_GATE,
            )
            .await?
            {
                return Ok(false);
            }
            crate::notification_outbox::runtime_attach::lock_runtime_attach_relations_after_gate_tx(
                &mut tx, schema,
            )
            .await?;
            if !crate::db::try_catalog_fence_lock(
                &mut tx,
                crate::push_service::runtime_attach::CUSTODY_GATE,
            )
            .await?
            {
                return Ok(false);
            }
            crate::push_service::runtime_attach::lock_runtime_attach_relations_after_gate_tx(
                &mut tx, schema,
            )
            .await?;
            crate::push_service::runtime_attach::lock_vapid_runtime_attach_relations_tx(
                &mut tx, schema,
            )
            .await?;
            runtime::lock_attested_table_tx(&mut tx, schema, &PUSH_REGISTRATIONS_CONTRACT).await?;
            if !crate::db::try_catalog_fence_lock(&mut tx, runtime::CUSTODY_GATE).await? {
                return Ok(false);
            }
            runtime::lock_attested_table_after_gate_tx(&mut tx, schema, &ACTIVITY_CONTRACT).await?;

            // Verify the security-definer helper only after attesting this
            // exact backend and before invoking it.
            capability.attest_transaction(&mut tx).await?;
            if !crate::db::try_catalog_fence_lock(
                &mut tx,
                crate::pubsub::runtime_attach::CUSTODY_GATE,
            )
            .await?
            {
                return Ok(false);
            }
            crate::pubsub::runtime_attach::lock_runtime_attach_relations_after_gate_tx(
                &mut tx, identity,
            )
            .await?;
            lock_foundation_descendants_tx(&mut tx, schema).await?;
            Ok::<_, PushNotificationCustodyError>(true)
        })
        .await
        .unwrap_or_else(|_| Err(DatabaseError::CatalogMutationLockTimeout.into()));
        if matches!(attempt, Ok(true)) {
            break tx;
        }
        if !tx.rollback_bounded_discard(CUSTODY_ROLLBACK_WAIT).await {
            return Err(DatabaseError::CatalogMutationRollbackUnconfirmed.into());
        }
        attempt?;
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return Err(DatabaseError::CatalogMutationGateContended.into());
        }
        tokio::time::sleep(CUSTODY_GATE_RETRY.min(remaining)).await;
        if tokio::time::Instant::now() >= deadline {
            return Err(DatabaseError::CatalogMutationGateContended.into());
        }
    };

    let snapshot = async {
        crate::notification_outbox::runtime_attach::verify_runtime_attach_tx(&mut tx, identity)
            .await?;
        crate::push_service::runtime_attach::verify_runtime_attach_tx(&mut tx, identity).await?;
        crate::push_service::runtime_attach::verify_vapid_custody_tx(&mut tx, identity, root_key)
            .await?;
        runtime::verify_attested_table_tx(&mut tx, identity, &PUSH_REGISTRATIONS_CONTRACT).await?;
        runtime::verify_attested_table_tx(&mut tx, identity, &ACTIVITY_CONTRACT).await?;
        crate::pubsub::runtime_attach::verify_runtime_attach_tx(&mut tx, identity).await?;

        // The sole application-row classification query takes one READ
        // COMMITTED command snapshot across all participating stores.
        read_custody_snapshot(&mut tx, schema).await
    }
    .await;
    if !tx.rollback_bounded_discard(CUSTODY_ROLLBACK_WAIT).await {
        return Err(DatabaseError::CatalogMutationRollbackUnconfirmed.into());
    }
    let (evidence, contradictory_rows) = snapshot?;
    if contradictory_rows != 0 {
        return Err(PushNotificationCustodyError::ContradictoryRows {
            rows: contradictory_rows,
        });
    }
    Ok(evidence)
}

async fn lock_foundation_descendants_tx(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(), PushNotificationCustodyError> {
    let quoted_schema = crate::db::physical_attestation::quote_identifier(schema);
    let qualified = format!("{quoted_schema}.\"{FOUNDATION_DESCENDANT_TABLE}\"");
    tx.execute(&format!("LOCK TABLE {qualified} IN ACCESS SHARE MODE"), ())
        .await
        .map_err(|source| PushNotificationCustodyError::FoundationRelation {
            table: FOUNDATION_DESCENDANT_TABLE,
            source,
        })?;
    Ok(())
}

struct CustodyCounts {
    classified_outbox_jobs: u64,
    unmatched_legacy_outbox_obligations: u64,
    classified_open_candidates: u64,
    unmatched_legacy_candidates: u64,
    classified_push_obligations: u64,
    unmatched_legacy_push_obligations: u64,
    unmatched_wire_push_obligations: u64,
    classified_foundation_descendants: u64,
    unmatched_legacy_foundation_descendants: u64,
    active_outbox_jobs: u64,
    parked_uncertain_outbox_jobs: u64,
    active_push_jobs: u64,
    parked_uncertain_push_jobs: u64,
    pending_notification_lineage_rows: u64,
    pending_foundation_descendants: u64,
    contradictory_rows: u64,
}

async fn read_custody_snapshot(
    tx: &mut Transaction<'_>,
    schema: &str,
) -> Result<(PushNotificationCustodyEvidence, u64), PushNotificationCustodyError> {
    let sql = custody_snapshot_sql(schema);
    let preview_kind = i64::from(
        waddle_xmpp::ingress::IngressEffectKind::NotificationActivityPreview.storage_tag(),
    );
    let recovery_kind = i64::from(
        waddle_xmpp::ingress::IngressEffectKind::GroupchatNotificationRecovery.storage_tag(),
    );
    let mut rows = tx
        .query(&sql, crate::db_params![preview_kind, recovery_kind])
        .await?;
    let row = rows
        .next()
        .await?
        .ok_or(PushNotificationCustodyError::InvalidSnapshot)?;
    let counts = CustodyCounts {
        classified_outbox_jobs: snapshot_count(&row, 0)?,
        unmatched_legacy_outbox_obligations: snapshot_count(&row, 1)?,
        classified_open_candidates: snapshot_count(&row, 2)?,
        unmatched_legacy_candidates: snapshot_count(&row, 3)?,
        classified_push_obligations: snapshot_count(&row, 4)?,
        unmatched_legacy_push_obligations: snapshot_count(&row, 5)?,
        unmatched_wire_push_obligations: snapshot_count(&row, 6)?,
        classified_foundation_descendants: snapshot_count(&row, 7)?,
        unmatched_legacy_foundation_descendants: snapshot_count(&row, 8)?,
        active_outbox_jobs: snapshot_count(&row, 9)?,
        parked_uncertain_outbox_jobs: snapshot_count(&row, 10)?,
        active_push_jobs: snapshot_count(&row, 11)?,
        parked_uncertain_push_jobs: snapshot_count(&row, 12)?,
        pending_notification_lineage_rows: snapshot_count(&row, 13)?,
        pending_foundation_descendants: snapshot_count(&row, 14)?,
        contradictory_rows: snapshot_count(&row, 15)?,
    };
    if rows.next().await?.is_some() {
        return Err(PushNotificationCustodyError::InvalidSnapshot);
    }
    Ok((
        PushNotificationCustodyEvidence {
            classified_outbox_jobs: counts.classified_outbox_jobs,
            unmatched_legacy_outbox_obligations: counts.unmatched_legacy_outbox_obligations,
            classified_open_candidates: counts.classified_open_candidates,
            unmatched_legacy_candidates: counts.unmatched_legacy_candidates,
            classified_push_obligations: counts.classified_push_obligations,
            unmatched_legacy_push_obligations: counts.unmatched_legacy_push_obligations,
            unmatched_wire_push_obligations: counts.unmatched_wire_push_obligations,
            classified_foundation_descendants: counts.classified_foundation_descendants,
            unmatched_legacy_foundation_descendants: counts.unmatched_legacy_foundation_descendants,
            active_outbox_jobs: counts.active_outbox_jobs,
            parked_uncertain_outbox_jobs: counts.parked_uncertain_outbox_jobs,
            active_push_jobs: counts.active_push_jobs,
            parked_uncertain_push_jobs: counts.parked_uncertain_push_jobs,
            pending_notification_lineage_rows: counts.pending_notification_lineage_rows,
            pending_foundation_descendants: counts.pending_foundation_descendants,
        },
        counts.contradictory_rows,
    ))
}

fn snapshot_count(row: &Row, column: usize) -> Result<u64, PushNotificationCustodyError> {
    let value = row.get::<i64>(column)?;
    u64::try_from(value).map_err(|_| PushNotificationCustodyError::InvalidSnapshot)
}

fn custody_snapshot_sql(schema: &str) -> String {
    let quoted = crate::db::physical_attestation::quote_identifier(schema);
    let relation = |name: &str| format!("{quoted}.\"{name}\"");
    let outbox = relation("notification_outbox");
    let candidates = relation("notification_candidates");
    let lineage = relation("notification_outbox_lineage");
    let descendants = relation(FOUNDATION_DESCENDANT_TABLE);
    let push_jobs = relation("push_publish_jobs");

    format!(
        r#"
        WITH
        notification_kinds(kind) AS (VALUES (?::integer), (?::integer)),
        lineage_by_job AS (
            SELECT job_id, count(*)::bigint AS row_count,
                   count(*) FILTER (WHERE settled_at_ms IS NULL)::bigint AS pending_count
            FROM {lineage}
            GROUP BY job_id
        ),
        lineage_by_candidate AS (
            SELECT candidate_delivery_id, count(*)::bigint AS row_count,
                   count(*) FILTER (WHERE settled_at_ms IS NULL)::bigint AS pending_count
            FROM {lineage}
            GROUP BY candidate_delivery_id
        ),
        foundation_by_key_parent AS (
            SELECT descendant_key, message_key, kind, semantic_identity_hash,
                   count(*)::bigint AS row_count,
                   count(*) FILTER (WHERE settled_at IS NULL)::bigint AS pending_count
            FROM {descendants}
            WHERE kind IN (SELECT kind FROM notification_kinds)
            GROUP BY descendant_key, message_key, kind, semantic_identity_hash
        ),
        foundation_by_key AS (
            SELECT descendant_key, sum(row_count)::bigint AS row_count,
                   sum(pending_count)::bigint AS pending_count
            FROM foundation_by_key_parent
            GROUP BY descendant_key
        ),
        foundation_by_job AS (
            SELECT job.descendant_key AS job_id,
                   sum(job.row_count)::bigint AS row_count,
                   sum(job.pending_count)::bigint AS pending_count
            FROM foundation_by_key_parent job
            WHERE EXISTS (
                SELECT 1
                FROM {lineage} l
                JOIN foundation_by_key_parent candidate
                  ON candidate.descendant_key = l.candidate_delivery_id
                 AND candidate.message_key = job.message_key
                 AND candidate.kind = job.kind
                 AND candidate.semantic_identity_hash = job.semantic_identity_hash
                WHERE l.job_id = job.descendant_key
            )
            GROUP BY job.descendant_key
        ),
        push_by_ancestry AS (
            SELECT ancestry_job_id AS job_id,
                   count(*)::bigint AS job_count,
                   count(*) FILTER (WHERE status IN ('queued', 'in-progress')
                     OR (status = 'failed' AND terminal_disposition IS NULL
                         AND uncertain_send = 1))::bigint AS obligation_count,
                   count(*) FILTER (WHERE status = 'failed' AND uncertain_send = 1
                     AND terminal_disposition IS NULL)::bigint AS parked_count,
                   count(*) FILTER (WHERE status = 'published'
                     OR terminal_disposition IS NOT NULL
                     OR (status = 'failed' AND uncertain_send = 0))::bigint AS terminal_count
            FROM {push_jobs}
            WHERE ancestry_job_id IS NOT NULL
            GROUP BY ancestry_job_id
        ),
        outbox_classification AS (
            SELECT
                count(*) FILTER (WHERE l.row_count IS NOT NULL
                                  OR f.row_count IS NOT NULL
                                  OR p.job_count IS NOT NULL)::bigint AS classified_count,
                count(*) FILTER (WHERE
                    (o.status IN ('queued', 'in-progress')
                     OR (o.status = 'failed' AND o.queue_acceptance_may_exist = 1))
                    AND l.row_count IS NULL AND f.row_count IS NULL
                    AND p.job_count IS NULL)::bigint AS unmatched_count,
                count(*) FILTER (WHERE o.status IN ('queued', 'in-progress'))::bigint
                    AS active_count,
                count(*) FILTER (WHERE o.status = 'failed'
                    AND o.queue_acceptance_may_exist = 1)::bigint AS parked_count,
                count(*) FILTER (WHERE (l.pending_count > 0 OR f.pending_count > 0)
                    AND p.terminal_count > 0)::bigint AS terminal_pending_count
            FROM {outbox} o
            LEFT JOIN lineage_by_job l ON l.job_id = o.job_id
            LEFT JOIN foundation_by_job f ON f.job_id = o.job_id
            LEFT JOIN push_by_ancestry p ON p.job_id = o.job_id
        ),
        candidate_classification AS (
            SELECT
                count(*) FILTER (WHERE c.outboxed_at_ms IS NULL
                    AND c.suppressed_reason IS NULL AND c.quarantined_at_ms IS NULL
                    AND (f.row_count > 0 OR l.row_count > 0))::bigint
                    AS classified_count,
                count(*) FILTER (WHERE c.outboxed_at_ms IS NULL
                    AND c.suppressed_reason IS NULL AND c.quarantined_at_ms IS NULL
                    AND f.row_count IS NULL AND l.row_count IS NULL)::bigint
                    AS unmatched_count
            FROM {candidates} c
            LEFT JOIN foundation_by_key f ON f.descendant_key = c.delivery_id
            LEFT JOIN lineage_by_candidate l ON l.candidate_delivery_id = c.delivery_id
        ),
        push_classification AS (
            SELECT
                count(*) FILTER (WHERE
                    (p.status IN ('queued', 'in-progress')
                    OR (p.status = 'failed' AND p.terminal_disposition IS NULL
                         AND p.uncertain_send = 1))
                    AND (o.job_id IS NOT NULL OR l.row_count IS NOT NULL
                         OR f.row_count IS NOT NULL))::bigint AS classified_count,
                count(*) FILTER (WHERE
                    (p.status IN ('queued', 'in-progress')
                     OR (p.status = 'failed' AND p.terminal_disposition IS NULL
                         AND p.uncertain_send = 1))
                    AND p.acceptance_scope = 'legacy'
                    AND o.job_id IS NULL AND l.row_count IS NULL
                    AND f.row_count IS NULL)::bigint AS unmatched_count,
                count(*) FILTER (WHERE
                    (p.status IN ('queued', 'in-progress')
                     OR (p.status = 'failed' AND p.terminal_disposition IS NULL
                         AND p.uncertain_send = 1))
                    AND p.acceptance_scope = 'wire'
                    AND o.job_id IS NULL AND l.row_count IS NULL
                    AND f.row_count IS NULL)::bigint AS unmatched_wire_count,
                count(*) FILTER (WHERE p.status IN ('queued', 'in-progress'))::bigint
                    AS active_count,
                count(*) FILTER (WHERE p.status = 'failed' AND p.uncertain_send = 1
                    AND p.terminal_disposition IS NULL)::bigint AS parked_count,
                count(*) FILTER (WHERE (l.pending_count > 0 OR f.pending_count > 0)
                    AND (p.status = 'published' OR p.terminal_disposition IS NOT NULL
                         OR (p.status = 'failed' AND p.uncertain_send = 0)))::bigint
                    AS terminal_pending_count
            FROM {push_jobs} p
            LEFT JOIN {outbox} o ON o.job_id = p.ancestry_job_id
            LEFT JOIN lineage_by_job l ON l.job_id = p.ancestry_job_id
            LEFT JOIN foundation_by_job f ON f.job_id = p.ancestry_job_id
        ),
        lineage_contradictions AS (
            SELECT count(*)::bigint AS row_count
            FROM {lineage} l
            LEFT JOIN {outbox} o ON o.job_id = l.job_id
            LEFT JOIN {candidates} c ON c.delivery_id = l.candidate_delivery_id
            WHERE l.settled_at_ms IS NULL AND (
                o.job_id IS NULL OR c.delivery_id IS NULL
                OR c.recipient_bare_jid IS DISTINCT FROM o.recipient_bare_jid
                OR c.conversation_jid IS DISTINCT FROM o.conversation_jid
                OR c.thread_id IS DISTINCT FROM o.thread_id
                OR c.class IS DISTINCT FROM o.class
            )
        ),
        candidate_delivery_duplicates AS (
            SELECT COALESCE(sum(duplicate_count - 1), 0)::bigint AS row_count
            FROM (
                SELECT count(*)::bigint AS duplicate_count
                FROM {candidates}
                WHERE delivery_id IS NOT NULL
                GROUP BY delivery_id
                HAVING count(*) > 1
            ) duplicates
        ),
        foundation_lineage_contradictions AS (
            SELECT count(*)::bigint AS row_count
            FROM {lineage} l
            JOIN foundation_by_key_parent candidate
              ON candidate.descendant_key = l.candidate_delivery_id
            WHERE l.settled_at_ms IS NULL
              AND NOT EXISTS (
                SELECT 1 FROM foundation_by_key_parent job
                WHERE job.descendant_key = l.job_id
                  AND job.message_key = candidate.message_key
                  AND job.kind = candidate.kind
                  AND job.semantic_identity_hash = candidate.semantic_identity_hash
                  AND job.pending_count > 0
              )
        ),
        foundation_classification AS (
            SELECT count(*)::bigint AS row_count
            FROM {descendants} d
            WHERE d.kind IN (SELECT kind FROM notification_kinds)
              AND d.settled_at IS NULL
              AND (EXISTS (SELECT 1 FROM {candidates} c
                           WHERE c.delivery_id = d.descendant_key)
                OR EXISTS (
                    SELECT 1
                    FROM {lineage} l
                    JOIN foundation_by_key_parent candidate
                      ON candidate.descendant_key = l.candidate_delivery_id
                    WHERE l.job_id = d.descendant_key
                      AND candidate.message_key = d.message_key
                      AND candidate.kind = d.kind
                      AND candidate.semantic_identity_hash = d.semantic_identity_hash
                ))
        ),
        push_contradictions AS (
            SELECT count(*) FILTER (WHERE p.acceptance_scope = 'canonical'
                    AND (p.ancestry_job_id IS NULL OR (
                      (p.status IN ('queued', 'in-progress')
                       OR (p.status = 'failed' AND p.terminal_disposition IS NULL
                           AND p.uncertain_send = 1))
                      AND NOT EXISTS (
                        SELECT 1 FROM {outbox} o
                        WHERE o.job_id = p.ancestry_job_id
                          AND o.recipient_bare_jid = p.owner_bare_jid
                          AND o.push_service_jid = p.push_service_jid
                          AND o.node = p.node
                          AND p.item_id = o.job_id
                          AND o.approved_payload_xml IS NOT NULL
                          AND p.payload_xml = o.approved_payload_xml
                          AND p.publish_options_xml
                              IS NOT DISTINCT FROM o.approved_publish_options_xml
                      ))))::bigint AS row_count
            FROM {push_jobs} p
        ),
        totals AS (
            SELECT
                o.classified_count AS classified_outbox_jobs,
                o.unmatched_count AS unmatched_outbox_obligations,
                c.classified_count AS classified_candidates,
                c.unmatched_count AS unmatched_candidates,
                p.classified_count AS classified_push_obligations,
                p.unmatched_count AS unmatched_push_obligations,
                p.unmatched_wire_count AS unmatched_wire_push_obligations,
                fc.row_count AS classified_foundation_descendants,
                (SELECT count(*) FILTER (WHERE settled_at IS NULL)::bigint
                   FROM {descendants} WHERE kind IN (SELECT kind FROM notification_kinds))
                  - fc.row_count AS unmatched_legacy_foundation_descendants,
                o.active_count AS active_outbox_jobs,
                o.parked_count AS parked_outbox_jobs,
                p.active_count AS active_push_jobs,
                p.parked_count AS parked_push_jobs,
                (SELECT count(*) FILTER (WHERE settled_at_ms IS NULL)::bigint FROM {lineage})
                    AS pending_lineage_rows,
                (SELECT count(*) FILTER (WHERE settled_at IS NULL)::bigint
                   FROM {descendants} WHERE kind IN (SELECT kind FROM notification_kinds))
                    AS pending_foundation_rows,
                o.terminal_pending_count + p.terminal_pending_count
                    + lc.row_count + cdc.row_count + flc.row_count + pc.row_count
                    AS contradictory_rows
            FROM outbox_classification o
            CROSS JOIN candidate_classification c
            CROSS JOIN push_classification p
            CROSS JOIN lineage_contradictions lc
            CROSS JOIN candidate_delivery_duplicates cdc
            CROSS JOIN foundation_lineage_contradictions flc
            CROSS JOIN foundation_classification fc
            CROSS JOIN push_contradictions pc
        )
        SELECT classified_outbox_jobs, unmatched_outbox_obligations,
               classified_candidates, unmatched_candidates,
               classified_push_obligations, unmatched_push_obligations,
               unmatched_wire_push_obligations,
               classified_foundation_descendants,
               unmatched_legacy_foundation_descendants,
               active_outbox_jobs, parked_outbox_jobs,
               active_push_jobs, parked_push_jobs,
               pending_lineage_rows, pending_foundation_rows, contradictory_rows
        FROM totals
        "#
    )
}

#[cfg(test)]
mod tests {
    use super::{read_custody_snapshot, PushNotificationCustodyEvidence};
    use crate::db::{Database, DatabaseConfig, DatabaseDriver};
    use anyhow::{ensure, Result};
    use sqlx::postgres::PgPool;

    fn postgres_url_with_search_path(database_url: &str, schema: &str) -> String {
        let mut url = url::Url::parse(database_url).expect("parse PostgreSQL test URL");
        let retained: Vec<(String, String)> = url
            .query_pairs()
            .filter(|(key, _)| key != "options")
            .map(|(key, value)| (key.into_owned(), value.into_owned()))
            .collect();
        url.query_pairs_mut()
            .clear()
            .extend_pairs(retained.iter().map(|(key, value)| (key, value)))
            .append_pair("options", &format!("-c search_path={schema}"));
        url.to_string()
    }

    async fn snapshot(
        database: &Database,
        schema: &str,
    ) -> Result<(PushNotificationCustodyEvidence, u64)> {
        let mut tx = database.begin().await?;
        tx.execute(
            "SET TRANSACTION ISOLATION LEVEL READ COMMITTED READ ONLY",
            (),
        )
        .await?;
        let snapshot = read_custody_snapshot(&mut tx, schema).await?;
        tx.rollback().await?;
        Ok(snapshot)
    }

    async fn exercise_classifier(admin: &PgPool, database: &Database, schema: &str) -> Result<()> {
        let canonical_job = "canonical-outbox-job";
        let canonical_candidate = "canonical-candidate";
        let payload = "<item xmlns='urn:test'>approved</item>";
        let message_key = "00000000-0000-4000-8000-000000000001";
        let preview_hash = vec![7_u8; 32];
        let recovery_hash = vec![21_u8; 32];

        sqlx::query(&format!(
            "INSERT INTO {schema}.notification_outbox \
             (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, \
              thread_id, class, approved_payload_xml, approved_publish_options_xml, \
              status, queue_acceptance_may_exist) \
             VALUES ($1, 'alice@example.test', 'push.example.test', 'web', \
                     'room@example.test', '', 'dm', $2, NULL, 'published', 0)"
        ))
        .bind(canonical_job)
        .bind(payload)
        .execute(admin)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO {schema}.notification_candidates \
             (stanza_id, delivery_id, recipient_bare_jid, conversation_jid, thread_id, \
              class, outboxed_at_ms, suppressed_reason, quarantined_at_ms) \
             VALUES ('canonical-stanza', $1, 'alice@example.test', \
                     'room@example.test', '', 'dm', 1, NULL, NULL)"
        ))
        .bind(canonical_candidate)
        .execute(admin)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO {schema}.notification_outbox_lineage \
             (candidate_delivery_id, job_id, settled_at_ms) VALUES ($1, $2, NULL)"
        ))
        .bind(canonical_candidate)
        .bind(canonical_job)
        .execute(admin)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO {schema}.ingress_messages (message_key) VALUES ($1::uuid)"
        ))
        .bind(message_key)
        .execute(admin)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO {schema}.ingress_effect_intents \
             (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) \
             VALUES ($1::uuid, 0, 7, $2, 1, $3), ($1::uuid, 1, 21, $4, 1, $3)"
        ))
        .bind(message_key)
        .bind(&preview_hash)
        .bind(b"{}".as_slice())
        .bind(&recovery_hash)
        .execute(admin)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO {schema}.ingress_effect_descendants \
             (message_key, kind, semantic_identity_hash, descendant_key, settled_at) \
             VALUES ($1::uuid, 7, $2, $3, now()), ($1::uuid, 7, $2, $4, NULL), \
                    ($1::uuid, 21, $5, 'synthetic-legacy-gap', NULL)"
        ))
        .bind(message_key)
        .bind(&preview_hash)
        .bind(canonical_candidate)
        .bind(canonical_job)
        .bind(&recovery_hash)
        .execute(admin)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO {schema}.push_publish_jobs \
             (job_id, ancestry_job_id, acceptance_scope, status, terminal_disposition, \
              uncertain_send, upstream_completed, owner_bare_jid, push_service_jid, node, \
              item_id, payload_xml, publish_options_xml) \
             VALUES ('canonical-push-job', $1, 'canonical', 'queued', NULL, 0, 1, \
                     'alice@example.test', 'push.example.test', 'web', $1, $2, NULL), \
                    ('legacy-active', NULL, 'legacy', 'queued', NULL, 0, 0, \
                     'legacy@example.test', NULL, 'old', 'legacy-item', '<legacy/>', NULL), \
                    ('wire-uncertain', NULL, 'wire', 'failed', NULL, 1, 0, \
                     'wire@example.test', NULL, 'wire', 'wire-item', '<wire/>', NULL)"
        ))
        .bind(canonical_job)
        .bind(payload)
        .execute(admin)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO {schema}.notification_outbox \
             (job_id, recipient_bare_jid, push_service_jid, node, conversation_jid, \
              thread_id, class, approved_payload_xml, approved_publish_options_xml, \
              status, queue_acceptance_may_exist) \
             VALUES ('uncertain-outbox', 'bob@example.test', 'push.example.test', \
                     'web', 'bob@example.test', '', 'dm', '<uncertain/>', NULL, 'failed', 1)"
        ))
        .execute(admin)
        .await?;

        let (evidence, contradictions) = snapshot(database, schema).await?;
        ensure!(
            contradictions == 0,
            "exact canonical binding is contradictory"
        );
        ensure!(
            evidence.classified_outbox_jobs == 1,
            "canonical outbox was not classified"
        );
        ensure!(
            evidence.unmatched_legacy_outbox_obligations == 1,
            "uncertain legacy outbox was not retained"
        );
        ensure!(
            evidence.classified_push_obligations == 1,
            "canonical Push obligation was not classified"
        );
        ensure!(
            evidence.unmatched_legacy_push_obligations == 1,
            "legacy Push obligation was not retained"
        );
        ensure!(
            evidence.unmatched_wire_push_obligations == 1,
            "wire Push obligation was not retained"
        );
        ensure!(
            evidence.active_push_jobs == 2,
            "active Push jobs were misclassified"
        );
        ensure!(
            evidence.parked_uncertain_push_jobs == 1,
            "failed uncertain Push job was not parked"
        );
        ensure!(
            evidence.parked_uncertain_outbox_jobs == 1,
            "failed uncertain outbox job was not parked"
        );
        ensure!(
            evidence.pending_notification_lineage_rows == 1,
            "pending notification lineage was lost"
        );
        ensure!(
            evidence.pending_foundation_descendants == 2,
            "pending Foundation descendants were lost"
        );
        ensure!(
            evidence.classified_foundation_descendants == 1,
            "known Foundation descendant was not classified"
        );
        ensure!(
            evidence.unmatched_legacy_foundation_descendants == 1,
            "synthetic legacy Foundation descendant was not retained"
        );

        sqlx::query(&format!(
            "INSERT INTO {schema}.notification_candidates \
             (stanza_id, delivery_id, recipient_bare_jid, conversation_jid, thread_id, \
              class, outboxed_at_ms, suppressed_reason, quarantined_at_ms) \
             VALUES ('wrong-owner-stanza', 'wrong-owner-candidate', 'mallory@example.test', \
                     'room@example.test', '', 'dm', 1, NULL, NULL)"
        ))
        .execute(admin)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO {schema}.notification_outbox_lineage \
             (candidate_delivery_id, job_id, settled_at_ms) VALUES \
             ('wrong-owner-candidate', $1, NULL)"
        ))
        .bind(canonical_job)
        .execute(admin)
        .await?;
        let (_, contradictions) = snapshot(database, schema).await?;
        ensure!(
            contradictions > 0,
            "pending Alice lineage to a different recipient must fail closed"
        );
        sqlx::query(&format!(
            "DELETE FROM {schema}.notification_outbox_lineage \
             WHERE candidate_delivery_id = 'wrong-owner-candidate'"
        ))
        .execute(admin)
        .await?;
        sqlx::query(&format!(
            "DELETE FROM {schema}.notification_candidates \
             WHERE stanza_id = 'wrong-owner-stanza'"
        ))
        .execute(admin)
        .await?;

        sqlx::query(&format!(
            "INSERT INTO {schema}.notification_candidates \
             (stanza_id, delivery_id, recipient_bare_jid, conversation_jid, thread_id, \
              class, outboxed_at_ms, suppressed_reason, quarantined_at_ms) \
             VALUES ('duplicate-stanza', $1, 'alice@example.test', \
                     'room@example.test', '', 'dm', 1, NULL, NULL)"
        ))
        .bind(canonical_candidate)
        .execute(admin)
        .await?;
        let (_, contradictions) = snapshot(database, schema).await?;
        ensure!(
            contradictions > 0,
            "duplicate non-null candidate delivery IDs must fail closed"
        );
        sqlx::query(&format!(
            "DELETE FROM {schema}.notification_candidates \
             WHERE stanza_id = 'duplicate-stanza'"
        ))
        .execute(admin)
        .await?;

        let unrelated_message = "00000000-0000-4000-8000-000000000002";
        let unrelated_hash = vec![8_u8; 32];
        sqlx::query(&format!(
            "INSERT INTO {schema}.ingress_messages (message_key) VALUES ($1::uuid)"
        ))
        .bind(unrelated_message)
        .execute(admin)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO {schema}.ingress_effect_intents \
             (message_key, effect_ordinal, kind, semantic_identity_hash, payload_version, payload) \
             VALUES ($1::uuid, 0, 7, $2, 1, $3)"
        ))
        .bind(unrelated_message)
        .bind(&unrelated_hash)
        .bind(b"{}".as_slice())
        .execute(admin)
        .await?;
        sqlx::query(&format!(
            "DELETE FROM {schema}.ingress_effect_descendants \
             WHERE descendant_key = $1 AND kind = 7 AND semantic_identity_hash = $2"
        ))
        .bind(canonical_job)
        .bind(&preview_hash)
        .execute(admin)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO {schema}.ingress_effect_descendants \
             (message_key, kind, semantic_identity_hash, descendant_key, settled_at) \
             VALUES ($1::uuid, 7, $2, $3, NULL)"
        ))
        .bind(unrelated_message)
        .bind(&unrelated_hash)
        .bind(canonical_job)
        .execute(admin)
        .await?;
        let (_, contradictions) = snapshot(database, schema).await?;
        ensure!(
            contradictions > 0,
            "Foundation descendant copied from an unrelated parent must fail closed"
        );
        sqlx::query(&format!(
            "DELETE FROM {schema}.ingress_effect_descendants \
             WHERE descendant_key = $1 AND kind = 7 AND semantic_identity_hash = $2"
        ))
        .bind(canonical_job)
        .bind(&unrelated_hash)
        .execute(admin)
        .await?;
        sqlx::query(&format!(
            "INSERT INTO {schema}.ingress_effect_descendants \
             (message_key, kind, semantic_identity_hash, descendant_key, settled_at) \
             VALUES ($1::uuid, 7, $2, $3, NULL)"
        ))
        .bind(message_key)
        .bind(&preview_hash)
        .bind(canonical_job)
        .execute(admin)
        .await?;

        sqlx::query(&format!(
            "UPDATE {schema}.ingress_effect_descendants SET settled_at = now() \
             WHERE descendant_key = $1 AND kind = 7 AND semantic_identity_hash = $2"
        ))
        .bind(canonical_job)
        .bind(&preview_hash)
        .execute(admin)
        .await?;
        let (_, contradictions) = snapshot(database, schema).await?;
        ensure!(
            contradictions > 0,
            "settled copied descendant cannot discharge a settled candidate parent with pending lineage"
        );
        sqlx::query(&format!(
            "UPDATE {schema}.ingress_effect_descendants SET settled_at = NULL \
             WHERE descendant_key = $1 AND kind = 7 AND semantic_identity_hash = $2"
        ))
        .bind(canonical_job)
        .bind(&preview_hash)
        .execute(admin)
        .await?;

        sqlx::query(&format!(
            "UPDATE {schema}.push_publish_jobs SET owner_bare_jid = 'wrong@example.test' \
             WHERE job_id = 'canonical-push-job'"
        ))
        .execute(admin)
        .await?;
        let (_, contradictions) = snapshot(database, schema).await?;
        ensure!(contradictions > 0, "wrong canonical owner must fail closed");
        sqlx::query(&format!(
            "UPDATE {schema}.push_publish_jobs SET owner_bare_jid = 'alice@example.test', \
             payload_xml = '<retargeted/>' WHERE job_id = 'canonical-push-job'"
        ))
        .execute(admin)
        .await?;
        let (_, contradictions) = snapshot(database, schema).await?;
        ensure!(
            contradictions > 0,
            "wrong canonical payload must fail closed"
        );
        sqlx::query(&format!(
            "UPDATE {schema}.push_publish_jobs SET payload_xml = $1 \
             WHERE job_id = 'canonical-push-job'"
        ))
        .bind(payload)
        .execute(admin)
        .await?;

        // A generic upstream ACK is not provider completion. Conversely, a
        // published Push row with unsettled ancestry is contradictory until
        // both Foundation and outbox lineage are settled.
        sqlx::query(&format!(
            "UPDATE {schema}.push_publish_jobs SET status = 'published' \
             WHERE job_id = 'canonical-push-job'"
        ))
        .execute(admin)
        .await?;
        let (evidence, contradictions) = snapshot(database, schema).await?;
        ensure!(
            contradictions > 0,
            "published Push with pending ancestry must fail closed"
        );
        ensure!(
            evidence.parked_uncertain_push_jobs == 1,
            "uncertain Push job was not parked alongside terminal work"
        );

        sqlx::query(&format!(
            "UPDATE {schema}.notification_outbox_lineage SET settled_at_ms = 1 \
             WHERE job_id = $1"
        ))
        .bind(canonical_job)
        .execute(admin)
        .await?;
        sqlx::query(&format!(
            "UPDATE {schema}.ingress_effect_descendants SET settled_at = now() \
             WHERE descendant_key = $1"
        ))
        .bind(canonical_job)
        .execute(admin)
        .await?;
        let (evidence, contradictions) = snapshot(database, schema).await?;
        ensure!(
            contradictions == 0,
            "settled terminal ancestry remains contradictory"
        );
        ensure!(
            evidence.pending_notification_lineage_rows == 0,
            "settled notification lineage remains pending"
        );
        ensure!(
            evidence.pending_foundation_descendants == 1,
            "unmatched Foundation gap was lost"
        );
        ensure!(
            evidence.unmatched_legacy_foundation_descendants == 1,
            "synthetic legacy Foundation descendant was not retained"
        );
        Ok(())
    }

    #[tokio::test]
    async fn postgres_joint_custody_classifier_preserves_and_fails_closed() {
        let Some(database_url) = waddle_ws_test_support::require_test_postgres_url() else {
            eprintln!("skipping joint custody classifier: PostgreSQL test URL not set");
            return;
        };
        let admin = PgPool::connect(&database_url)
            .await
            .expect("connect PostgreSQL 17 test cluster");
        let version: i32 =
            sqlx::query_scalar("SELECT current_setting('server_version_num')::integer")
                .fetch_one(&admin)
                .await
                .expect("read PostgreSQL version");
        assert!(
            version >= 170_000,
            "joint custody test requires PostgreSQL 17+"
        );
        let schema = format!(
            "push_notification_custody_{}",
            uuid::Uuid::new_v4().simple()
        );
        sqlx::query(&format!("CREATE SCHEMA {schema}"))
            .execute(&admin)
            .await
            .expect("create isolated custody schema");

        let mut config = DatabaseConfig::new(
            DatabaseDriver::Postgres,
            postgres_url_with_search_path(&database_url, &schema),
        );
        config.pool_size = 1;
        let database = Database::from_config("push-notification custody classifier test", &config)
            .await
            .expect("open isolated custody test database");
        let table_ddl = [
            format!(
                "CREATE TABLE {schema}.notification_outbox (\
                   job_id TEXT PRIMARY KEY, recipient_bare_jid TEXT NOT NULL, \
                   push_service_jid TEXT NOT NULL, node TEXT NOT NULL, \
                   conversation_jid TEXT NOT NULL, thread_id TEXT NOT NULL, class TEXT NOT NULL, \
                   approved_payload_xml TEXT, approved_publish_options_xml TEXT, \
                   status TEXT NOT NULL, queue_acceptance_may_exist INTEGER NOT NULL)"
            ),
            format!(
                "CREATE TABLE {schema}.notification_candidates (\
                   stanza_id TEXT, delivery_id TEXT, recipient_bare_jid TEXT NOT NULL, \
                   conversation_jid TEXT NOT NULL, thread_id TEXT NOT NULL, class TEXT NOT NULL, \
                   outboxed_at_ms BIGINT, suppressed_reason TEXT, quarantined_at_ms BIGINT)"
            ),
            format!(
                "CREATE TABLE {schema}.notification_outbox_lineage (\
                   candidate_delivery_id TEXT NOT NULL, job_id TEXT NOT NULL, \
                   settled_at_ms BIGINT)"
            ),
            format!("CREATE TABLE {schema}.ingress_messages (message_key UUID PRIMARY KEY)"),
            format!(
                "CREATE TABLE {schema}.ingress_effect_intents (\
                   message_key UUID NOT NULL REFERENCES {schema}.ingress_messages(message_key) \
                     ON DELETE CASCADE, \
                   effect_ordinal NUMERIC(20,0) NOT NULL, kind INTEGER NOT NULL CHECK (kind >= 0), \
                   semantic_identity_hash BYTEA NOT NULL \
                     CHECK (octet_length(semantic_identity_hash) = 32), \
                   payload_version INTEGER NOT NULL CHECK (payload_version = 1), \
                   payload BYTEA NOT NULL CHECK (octet_length(payload) <= 65536), \
                   PRIMARY KEY (message_key, kind, semantic_identity_hash))"
            ),
            format!(
                "CREATE TABLE {schema}.ingress_effect_descendants (\
                   message_key UUID NOT NULL, kind INTEGER NOT NULL, \
                   semantic_identity_hash BYTEA NOT NULL \
                     CHECK (octet_length(semantic_identity_hash) = 32), \
                   descendant_key TEXT NOT NULL, settled_at TIMESTAMPTZ, \
                   PRIMARY KEY (message_key, kind, semantic_identity_hash, descendant_key), \
                   FOREIGN KEY (message_key, kind, semantic_identity_hash) \
                     REFERENCES {schema}.ingress_effect_intents \
                       (message_key, kind, semantic_identity_hash) ON DELETE CASCADE)"
            ),
            format!(
                "CREATE TABLE {schema}.push_publish_jobs (\
                   job_id TEXT PRIMARY KEY, ancestry_job_id TEXT, acceptance_scope TEXT NOT NULL, \
                   status TEXT NOT NULL, terminal_disposition TEXT, uncertain_send INTEGER NOT NULL, \
                   upstream_completed INTEGER NOT NULL, owner_bare_jid TEXT NOT NULL, \
                   push_service_jid TEXT, node TEXT NOT NULL, item_id TEXT NOT NULL, \
                   payload_xml TEXT NOT NULL, publish_options_xml TEXT)"
            ),
        ];
        let setup = async {
            for ddl in table_ddl {
                sqlx::query(&ddl).execute(&admin).await?;
            }
            exercise_classifier(&admin, &database, &schema).await
        }
        .await;

        drop(database);
        sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
            .execute(&admin)
            .await
            .expect("drop isolated custody schema");
        admin.close().await;
        setup.expect("joint custody one-statement classification scenarios");
    }
}
