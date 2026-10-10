//! Durable publish-job queue state: enqueue/upsert, claim + stale-claim
//! recovery, retry bookkeeping, retention pruning, and row decoding for
//! jobs and delivery attempts.

use jid::BareJid;
use minidom::Element;
use waddle_xmpp::pubsub::PubSubItem;
use waddle_xmpp::XmppError;

use super::devices::validate_len;
use super::nodes::get_node_tx;
use super::pubsub_backing::validate_xep0357_notification;
use super::registration::ensure_active_registration_tx;
use super::store::{lock_node_tx, lock_owner_tx, DatabasePushServiceStore};
use super::types::{
    PushAcceptanceScope, PushBackingState, PushDeliveryAttempt, PushNodeStatus, PushPublishJob,
    PushPublishJobEnqueue,
};

pub(super) const PUBLISH_JOB_STATUS_QUEUED: &str = "queued";

pub(super) const PUBLISH_JOB_STATUS_IN_PROGRESS: &str = "in-progress";

pub(super) const PUBLISH_JOB_STATUS_PUBLISHED: &str = "published";

pub(super) const PUBLISH_JOB_STATUS_FAILED: &str = "failed";

pub(super) const PUBLISH_JOB_ERROR_NO_ACTIVE_DEVICES: &str = "Push node has no active devices";

pub(super) const MAX_DELIVERY_ATTEMPTS_PER_NODE: i64 = 10_000;

pub(super) const MAX_PUBLISH_JOBS_PER_NODE: i64 = 10_000;

pub(super) const MAX_PUBSUB_ITEM_ID_LEN: usize = 256;

pub(super) const PUBLISH_JOB_RETRY_DELAY_MS: i64 = 60_000;

/// Upper bound on one claim/dispatch/finalize pass. Expired claims fence
/// bookkeeping through a fresh token; a provider send whose reply was lost
/// remains retryable and duplicate-possible because APNs/Web Push do not
/// accept the Foundation delivery key as an idempotency contract.
pub(super) const PUBLISH_JOB_CLAIM_TIMEOUT_MS: i64 = 30 * 60 * 1_000; // 30 minutes

/// Ceiling on transient retries before a publish job is marked
/// `failed`. XEP-0357 §6 explicitly contemplates this: "a server MAY
/// choose to keep a service enabled if the error is deemed recoverable
/// or transient, until a sufficient number of errors have been received
/// in a row." 24 attempts × 60s = a 24-minute upper bound on retry
/// noise per job before the operator's `last_error` audit reveals the
/// underlying problem.
pub(super) const PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS: i64 = 24;

/// Hard ceiling on `Retry-After`-derived backoff to prevent a
/// misbehaving relay from pinning a job into an effectively-forever
/// requeue. 1 hour comfortably covers any sane rate-limit window.
pub(super) const PUBLISH_JOB_MAX_RETRY_AFTER_MS: i64 = 60 * 60 * 1_000;

pub(super) async fn allocate_publication_order_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
) -> Result<i64, XmppError> {
    tx.execute("INSERT INTO push_publication_orders (node, next_order) VALUES (?, 0) ON CONFLICT DO NOTHING", crate::db_params![node]).await.map_err(|error| XmppError::internal(error.to_string()))?;
    let changed = tx.execute("UPDATE push_publication_orders SET next_order = next_order + 1 WHERE node = ? AND next_order < ?", crate::db_params![node, i64::MAX]).await.map_err(|error| XmppError::internal(error.to_string()))?;
    if changed == 0 {
        return Err(XmppError::internal("publication order exhausted"));
    }
    let mut rows = tx
        .query(
            "SELECT next_order FROM push_publication_orders WHERE node = ?",
            crate::db_params![node],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let row = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
        .ok_or_else(|| XmppError::internal("publication order missing"))?;
    let order: i64 = row
        .get(0)
        .map_err(|error| XmppError::internal(error.to_string()))?;
    if order < 1 {
        return Err(XmppError::internal("invalid publication order counter"));
    }
    Ok(order)
}

pub(super) async fn wake_queued_publish_jobs_for_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
    now_ms: i64,
) -> Result<(), XmppError> {
    tx.execute(
        r#"
        UPDATE push_publish_jobs
        SET next_retry_at_ms = NULL,
            updated_at_ms = ?
        WHERE node = ?
          AND status = ?
          AND last_error = ?
        "#,
        crate::db_params![
            now_ms,
            node,
            PUBLISH_JOB_STATUS_QUEUED,
            PUBLISH_JOB_ERROR_NO_ACTIVE_DEVICES,
        ],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

pub(super) async fn claim_publish_job_tx(
    tx: &mut crate::db::Transaction<'_>,
    job_id: &str,
    now_ms: i64,
) -> Result<Option<PushPublishJob>, XmppError> {
    // Mint a fresh claim_token on every claim. Phase 3's UPDATE gates
    // on this token so a stale-claim recovery + concurrent re-claim
    // can never persist attempts from the original worker — the
    // original worker's token is no longer the row's token.
    let claim_token = uuid::Uuid::new_v4().to_string();
    let changed = tx
        .execute(
            r#"
            UPDATE push_publish_jobs
            SET status = ?,
                claimed_at_ms = ?,
                claim_token = ?,
                updated_at_ms = ?
            WHERE job_id = ?
              AND status = ?
              AND (next_retry_at_ms IS NULL OR next_retry_at_ms <= ?)
            "#,
            crate::db_params![
                PUBLISH_JOB_STATUS_IN_PROGRESS,
                now_ms,
                claim_token,
                now_ms,
                job_id,
                PUBLISH_JOB_STATUS_QUEUED,
                now_ms,
            ],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    if changed == 0 {
        return Ok(None);
    }
    get_publish_job_tx(tx, job_id).await
}

pub(super) async fn get_publish_job_tx(
    tx: &mut crate::db::Transaction<'_>,
    job_id: &str,
) -> Result<Option<PushPublishJob>, XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT job_id, owner_bare_jid, node, item_id, push_service_jid, status, claim_token, ancestry_job_id, acceptance_scope, publication_order, backing_state, uncertain_send
            FROM push_publish_jobs
            WHERE job_id = ?
            "#,
            crate::db_params![job_id],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    Ok(Some(decode_publish_job(&row)?))
}

/// Read the persisted XEP-0357 `<notification>` payload XML for a
/// publish-job. The worker uses this between tx1 (claim+load) and tx2
/// (record attempts) so the actual Web Push dispatch happens outside any
/// DB transaction.
pub(super) async fn get_publish_job_payload_xml_tx(
    tx: &mut crate::db::Transaction<'_>,
    job_id: &str,
) -> Result<Option<String>, XmppError> {
    let mut rows = tx
        .query(
            "SELECT payload_xml FROM push_publish_jobs WHERE job_id = ?",
            crate::db_params![job_id],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    let payload_xml: String = row
        .get(0)
        .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(Some(payload_xml))
}

/// Device ids that already recorded a terminal-success attempt for
/// this exact scheduler acceptance — `web-delivered` for real Web Push sends,
/// `apns-delivered` for real APNs sends, `fake-sent` for the stubbed
/// FCM platform. A retried publish
/// job filters its fan-out against this set so one transiently
/// failing sibling does not turn into duplicate OS notifications on
/// every device that already received the item (#1123).
pub(super) async fn delivered_device_ids_for_acceptance_tx(
    tx: &mut crate::db::Transaction<'_>,
    publish_job_id: &str,
) -> Result<std::collections::HashSet<String>, XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT DISTINCT device_id
            FROM push_delivery_attempts
            WHERE publish_job_id = ? AND status IN (?, ?, ?)
            "#,
            crate::db_params![
                publish_job_id,
                super::dispatch::ATTEMPT_STATUS_WEB_DELIVERED,
                super::apns_dispatch::ATTEMPT_STATUS_APNS_DELIVERED,
                super::dispatch::ATTEMPT_STATUS_FAKE_SENT_NON_WEB,
            ],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let mut delivered = std::collections::HashSet::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    {
        let device_id: String = row
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))?;
        delivered.insert(device_id);
    }
    Ok(delivered)
}

/// Read the row's current `claim_token` so phase 3 can verify the
/// claim is still ours before persisting any side effects. Returns
/// `None` when the row was recovered (token cleared) or deleted.
pub(super) async fn read_publish_job_claim_token_tx(
    tx: &mut crate::db::Transaction<'_>,
    job_id: &str,
) -> Result<Option<String>, XmppError> {
    let mut rows = tx
        .query(
            "SELECT claim_token FROM push_publish_jobs WHERE job_id = ?",
            crate::db_params![job_id],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    let token: Option<String> = row
        .get(0)
        .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(token)
}

/// Read the persisted `attempt_count` for a job. Used by phase 3 to
/// enforce [`PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS`] (XEP-0357 §6.1).
pub(super) async fn read_publish_job_attempt_count_tx(
    tx: &mut crate::db::Transaction<'_>,
    job_id: &str,
) -> Result<Option<i64>, XmppError> {
    let mut rows = tx
        .query(
            "SELECT attempt_count FROM push_publish_jobs WHERE job_id = ?",
            crate::db_params![job_id],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    else {
        return Ok(None);
    };
    row.get(0)
        .map(Some)
        .map_err(|error| XmppError::internal(error.to_string()))
}

pub(super) async fn mark_publish_job_failed_tx(
    tx: &mut crate::db::Transaction<'_>,
    job_id: &str,
    error: &str,
    now_ms: i64,
) -> Result<(), XmppError> {
    let job = get_publish_job_tx(tx, job_id).await?;
    tx.execute(
        r#"
        UPDATE push_publish_jobs
        SET status = ?,
            attempt_count = attempt_count + 1,
            last_error = ?,
            next_retry_at_ms = NULL,
            claimed_at_ms = NULL,
            updated_at_ms = ?
        WHERE job_id = ?
        "#,
        crate::db_params![PUBLISH_JOB_STATUS_FAILED, error, now_ms, job_id],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    if let Some(job) = job {
        settle_notification_ancestry_tx(tx, &job).await?;
    }
    Ok(())
}

async fn has_notification_lineage_tx(
    tx: &mut crate::db::Transaction<'_>,
) -> Result<bool, XmppError> {
    Ok(if tx.driver() == crate::db::DatabaseDriver::Postgres {
        let mut rows = tx
            .query(
                "SELECT to_regclass('notification_outbox_lineage')::text",
                (),
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        match rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            Some(row) => row
                .get::<Option<String>>(0)
                .map_err(|error| XmppError::internal(error.to_string()))?
                .is_some(),
            None => false,
        }
    } else {
        let mut rows = tx.query("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'notification_outbox_lineage'", ()).await
                .map_err(|error| XmppError::internal(error.to_string()))?;
        rows.next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
            .is_some()
    })
}

pub(super) async fn lock_notification_ancestry_tx(
    tx: &mut crate::db::Transaction<'_>,
    job: &PushPublishJob,
) -> Result<(), XmppError> {
    lock_notification_ancestry_typed_tx(tx, job)
        .await
        .map_err(|error| match error {
            NotificationAncestryLockError::Contended => {
                XmppError::internal("notification ancestry unavailable")
            }
            NotificationAncestryLockError::Failed(error) => error,
        })
}

enum NotificationAncestryLockError {
    Contended,
    Failed(XmppError),
}

impl From<crate::ingress_uow::IngressUowError> for NotificationAncestryLockError {
    fn from(error: crate::ingress_uow::IngressUowError) -> Self {
        if error.retry_class() == crate::ingress_uow::DbRetryClass::CanonicalLockContention {
            Self::Contended
        } else {
            Self::Failed(XmppError::internal("notification ancestry unavailable"))
        }
    }
}

impl From<crate::db::DatabaseError> for NotificationAncestryLockError {
    fn from(error: crate::db::DatabaseError) -> Self {
        if let crate::db::DatabaseError::Internal(sqlx::Error::Database(database)) = &error {
            if database.code().as_deref() == Some("55P03") {
                return Self::Contended;
            }
        }
        Self::Failed(XmppError::internal(error.to_string()))
    }
}

async fn lock_notification_ancestry_typed_tx(
    tx: &mut crate::db::Transaction<'_>,
    job: &PushPublishJob,
) -> Result<(), NotificationAncestryLockError> {
    if let Some(id) = job.ancestry_job_id() {
        crate::ingress_uow::EffectDescendantRepository::lock_all_nowait_raw(tx, id).await?;
        if has_notification_lineage_tx(tx)
            .await
            .map_err(NotificationAncestryLockError::Failed)?
        {
            let sql = if tx.driver() == crate::db::DatabaseDriver::Postgres {
                "SELECT candidate_delivery_id FROM notification_outbox_lineage WHERE job_id = ? AND settled_at_ms IS NULL ORDER BY candidate_delivery_id FOR UPDATE"
            } else {
                "SELECT candidate_delivery_id FROM notification_outbox_lineage WHERE job_id = ? AND settled_at_ms IS NULL ORDER BY candidate_delivery_id"
            };
            let mut rows = tx.query(sql, crate::db_params![id.to_string()]).await?;
            while rows.next().await?.is_some() {}
            crate::ingress_uow::EffectDescendantRepository::lock_all_nowait_raw(tx, id).await?;
        }
    }
    Ok(())
}

pub(super) async fn settle_notification_ancestry_tx(
    tx: &mut crate::db::Transaction<'_>,
    job: &PushPublishJob,
) -> Result<(), XmppError> {
    if let Some(id) = job.ancestry_job_id() {
        crate::ingress_uow::EffectDescendantRepository::settle_all_raw(tx, id, chrono::Utc::now())
            .await
            .map_err(|_| XmppError::internal("notification ancestry settlement unavailable"))?;
        if has_notification_lineage_tx(tx).await? {
            tx.execute("UPDATE notification_outbox_lineage SET settled_at_ms = ? WHERE job_id = ? AND settled_at_ms IS NULL",
                crate::db_params![crate::time::now_ms(), id.to_string()]).await
                .map_err(|error| XmppError::internal(error.to_string()))?;
        }
    }
    Ok(())
}

pub(super) async fn settle_terminal_notification_ancestry_tx(
    tx: &mut crate::db::Transaction<'_>,
    job: &PushPublishJob,
) -> Result<(), XmppError> {
    let mut rows = tx
        .query(
            "SELECT status FROM push_publish_jobs WHERE job_id = ?",
            crate::db_params![job.job_id()],
        )
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?;
    let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    else {
        return Ok(());
    };
    let status: String = row
        .get(0)
        .map_err(|error| XmppError::internal(error.to_string()))?;
    if matches!(
        status.as_str(),
        PUBLISH_JOB_STATUS_PUBLISHED | PUBLISH_JOB_STATUS_FAILED
    ) {
        settle_notification_ancestry_tx(tx, job).await?;
    }
    Ok(())
}

pub(super) async fn prune_delivery_attempts_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
    limit: i64,
) -> Result<(), XmppError> {
    // Terminal-success attempts of a still-retryable job are exempt
    // from the retention tail (#1123, Greptile review): the per-device
    // idempotency filter reads `web-delivered`/`apns-delivered`/
    // `fake-sent` rows for the job's `(node, item_id)` on every retry,
    // so evicting one mid-retry would re-push the item to a device that
    // already received it. Only that narrow slice is protected — failure/
    // transient attempts (pure audit) and attempts of terminal jobs
    // (published/failed/deleted — no re-dispatch to protect) prune
    // normally.
    tx.execute(
        r#"
        DELETE FROM push_delivery_attempts
        WHERE node = ?
          AND attempt_id NOT IN (
              SELECT attempt_id
              FROM push_delivery_attempts
              WHERE node = ?
              ORDER BY created_at_ms DESC, attempt_id DESC
              LIMIT ?
          )
          AND NOT (
              publish_job_id IS NOT NULL
              AND status IN (?, ?, ?)
              AND publish_job_id IN (
                  SELECT job_id
                  FROM push_publish_jobs
                  WHERE node = ?
                    AND status IN (?, ?)
              )
          )
        "#,
        crate::db_params![
            node,
            node,
            limit,
            super::dispatch::ATTEMPT_STATUS_WEB_DELIVERED,
            super::apns_dispatch::ATTEMPT_STATUS_APNS_DELIVERED,
            super::dispatch::ATTEMPT_STATUS_FAKE_SENT_NON_WEB,
            node,
            PUBLISH_JOB_STATUS_QUEUED,
            PUBLISH_JOB_STATUS_IN_PROGRESS,
        ],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

pub(super) async fn prune_publish_jobs_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
    limit: i64,
) -> Result<(), XmppError> {
    let has_foundation = if tx.driver() == crate::db::DatabaseDriver::Postgres {
        let mut rows = tx
            .query("SELECT to_regclass('ingress_effect_descendants')::text", ())
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        match rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            Some(row) => row
                .get::<Option<String>>(0)
                .map_err(|error| XmppError::internal(error.to_string()))?
                .is_some(),
            None => false,
        }
    } else {
        let mut rows = tx.query("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'ingress_effect_descendants'", ()).await.map_err(|error| XmppError::internal(error.to_string()))?;
        rows.next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
            .is_some()
    };
    let ancestry_guard = if has_foundation {
        "AND NOT EXISTS (SELECT 1 FROM ingress_effect_descendants WHERE descendant_key = push_publish_jobs.ancestry_job_id)"
    } else {
        ""
    };
    tx.execute(
        &format!(
            r#"
        DELETE FROM push_publish_jobs
        WHERE node = ?
          {ancestry_guard}
          AND status IN (?, ?)
          AND updated_at_ms <= ?
          AND job_id NOT IN (
              SELECT job_id
              FROM push_publish_jobs
              WHERE node = ?
              ORDER BY created_at_ms DESC, job_id DESC
              LIMIT ?
        )
        "#
        ),
        crate::db_params![
            node,
            PUBLISH_JOB_STATUS_PUBLISHED,
            PUBLISH_JOB_STATUS_FAILED,
            crate::time::now_ms().saturating_sub(8 * 24 * 60 * 60 * 1_000),
            node,
            limit
        ],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

pub(super) async fn cancel_retryable_publish_jobs_for_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    node: &str,
) -> Result<(), XmppError> {
    let mut rows = tx.query(
        "SELECT job_id, owner_bare_jid, node, item_id, push_service_jid, status, claim_token, ancestry_job_id, acceptance_scope, publication_order, backing_state, uncertain_send FROM push_publish_jobs WHERE owner_bare_jid = ? AND node = ? AND status IN (?, ?) ORDER BY job_id",
        crate::db_params![owner_bare_jid.to_string(), node, PUBLISH_JOB_STATUS_QUEUED, PUBLISH_JOB_STATUS_IN_PROGRESS],
    ).await.map_err(|error| XmppError::internal(error.to_string()))?;
    let mut jobs = Vec::new();
    while let Some(row) = rows
        .next()
        .await
        .map_err(|error| XmppError::internal(error.to_string()))?
    {
        jobs.push(decode_publish_job(&row)?);
    }
    for job in jobs {
        // Node/owner locks are already held by revocation. NOWAIT avoids
        // reversing Foundation's canonical-before-scheduler lock order.
        lock_notification_ancestry_tx(tx, &job).await?;
        tx.execute("UPDATE push_publish_jobs SET status = ?, last_error = ?, next_retry_at_ms = NULL, claimed_at_ms = NULL, claim_token = NULL, updated_at_ms = ? WHERE job_id = ? AND status IN (?, ?)",
            crate::db_params![PUBLISH_JOB_STATUS_FAILED, "notification registration revoked", crate::time::now_ms(), job.job_id(), PUBLISH_JOB_STATUS_QUEUED, PUBLISH_JOB_STATUS_IN_PROGRESS],
        ).await.map_err(|error| XmppError::internal(error.to_string()))?;
        settle_notification_ancestry_tx(tx, &job).await?;
    }
    Ok(())
}

pub(super) fn retry_at_ms(now_ms: i64) -> i64 {
    // #1126: ±25% jitter so publish jobs requeued by one relay outage
    // do not all retry on the same 60s beat.
    let jitter = {
        use rand::RngExt as _;
        let factor: f64 = rand::rng().random_range(0.75..=1.25);
        ((PUBLISH_JOB_RETRY_DELAY_MS as f64) * factor) as i64
    };
    now_ms.saturating_add(jitter)
}

fn decode_publish_job(row: &crate::db::Row) -> Result<PushPublishJob, XmppError> {
    let owner_bare_jid: String = row
        .get(1)
        .map_err(|error| XmppError::internal(error.to_string()))?;
    // The `claim_token` column is nullable for legacy / unclaimed
    // rows; treat NULL as empty string. Phase 3 only uses it for the
    // gating UPDATE on rows it itself just claimed, so an empty
    // token never matches a real claim.
    let claim_token: Option<String> = row
        .get(6)
        .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(PushPublishJob {
        job_id: row
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        owner_bare_jid: owner_bare_jid.parse().map_err(|error| {
            XmppError::internal(format!(
                "Invalid stored push publish job owner JID: {error}"
            ))
        })?,
        node: row
            .get(2)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        item_id: row
            .get(3)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        push_service_jid: row
            .get::<Option<String>>(4)
            .map_err(|error| XmppError::internal(error.to_string()))?
            .map(|raw| raw.parse::<BareJid>())
            .transpose()
            .map_err(|_| XmppError::internal("invalid stored push service JID"))?,
        status: row
            .get(5)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        claim_token: claim_token.unwrap_or_default(),
        ancestry_job_id: row
            .get::<Option<String>>(7)
            .map_err(|error| XmppError::internal(error.to_string()))?
            .map(|raw| uuid::Uuid::parse_str(&raw))
            .transpose()
            .map_err(|_| XmppError::internal("invalid notification ancestry binding"))?,
        acceptance_scope: PushAcceptanceScope::from_db(
            &row.get::<String>(8)
                .map_err(|error| XmppError::internal(error.to_string()))?,
        )?,
        publication_order: u64::try_from(
            row.get::<i64>(9)
                .map_err(|error| XmppError::internal(error.to_string()))?,
        )
        .map_err(|_| XmppError::internal("invalid publication order"))?,
        backing_state: PushBackingState::from_db(
            &row.get::<String>(10)
                .map_err(|error| XmppError::internal(error.to_string()))?,
        )?,
        uncertain_send: row
            .get::<i64>(11)
            .map_err(|error| XmppError::internal(error.to_string()))?
            != 0,
    })
}

fn decode_attempt(row: &crate::db::Row) -> Result<PushDeliveryAttempt, XmppError> {
    Ok(PushDeliveryAttempt {
        attempt_id: row
            .get(0)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        node: row
            .get(1)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        device_id: row
            .get(2)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        item_id: row
            .get(3)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        status: row
            .get(4)
            .map_err(|error| XmppError::internal(error.to_string()))?,
    })
}

impl DatabasePushServiceStore {
    pub(super) async fn load_publish_job(
        &self,
        id: &str,
    ) -> Result<Option<PushPublishJob>, XmppError> {
        let mut tx = self
            .db
            .begin()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let job = get_publish_job_tx(&mut tx, id).await?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(job)
    }
    pub(crate) async fn adopt_notification_ancestry(&self) -> Result<(), XmppError> {
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        if has_notification_lineage_tx(&mut tx).await? {
            tx.execute("UPDATE push_publish_jobs SET ancestry_job_id = (SELECT job_id FROM notification_outbox WHERE notification_outbox.job_id = push_publish_jobs.item_id AND notification_outbox.recipient_bare_jid = push_publish_jobs.owner_bare_jid AND notification_outbox.node = push_publish_jobs.node AND notification_outbox.push_service_jid = push_publish_jobs.push_service_jid AND (notification_outbox.approved_payload_xml IS NULL OR (notification_outbox.approved_payload_xml = push_publish_jobs.payload_xml AND (notification_outbox.approved_publish_options_xml = push_publish_jobs.publish_options_xml OR (notification_outbox.approved_publish_options_xml IS NULL AND push_publish_jobs.publish_options_xml IS NULL))))) WHERE ancestry_job_id IS NULL AND acceptance_scope = 'legacy' AND EXISTS (SELECT 1 FROM notification_outbox WHERE notification_outbox.job_id = push_publish_jobs.item_id AND notification_outbox.recipient_bare_jid = push_publish_jobs.owner_bare_jid AND notification_outbox.node = push_publish_jobs.node AND notification_outbox.push_service_jid = push_publish_jobs.push_service_jid)", ()).await.map_err(|error| XmppError::internal(error.to_string()))?;
        }
        tx.execute("UPDATE push_publish_jobs SET acceptance_scope = 'canonical' WHERE ancestry_job_id IS NOT NULL AND acceptance_scope = 'legacy'", ()).await.map_err(|error| XmppError::internal(error.to_string()))?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))
    }
    #[cfg(test)]
    pub(super) async fn enqueue_notification_publish_job_from_user_server(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
    ) -> Result<PushPublishJobEnqueue, XmppError> {
        self.enqueue_notification_publish_job_from_user_server_with_publish_options(
            node, item, publisher, None, None,
        )
        .await
    }

    pub(super) async fn enqueue_notification_publish_job_from_user_server_with_publish_options(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        push_service_jid: Option<&str>,
        publish_options: Option<&Element>,
    ) -> Result<PushPublishJobEnqueue, XmppError> {
        let service = push_service_jid
            .map(|raw| raw.parse::<BareJid>())
            .transpose()
            .map_err(|_| XmppError::bad_request(Some("invalid push service JID".to_string())))?;
        self.enqueue_notification_publish_job(
            node,
            item,
            publisher,
            service.as_ref(),
            publish_options,
            None,
        )
        .await
    }

    pub(super) async fn enqueue_canonical_notification_publish_job(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        service: &BareJid,
        options: Option<&Element>,
        delivery: uuid::Uuid,
    ) -> Result<PushPublishJobEnqueue, XmppError> {
        self.enqueue_notification_publish_job(
            node,
            item,
            publisher,
            Some(service),
            options,
            Some(delivery),
        )
        .await
    }

    async fn enqueue_notification_publish_job(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        push_service_jid: Option<&BareJid>,
        publish_options: Option<&Element>,
        canonical_delivery_id: Option<uuid::Uuid>,
    ) -> Result<PushPublishJobEnqueue, XmppError> {
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let now_ms = crate::time::now_ms();
        lock_owner_tx(&mut tx, publisher, now_ms).await?;
        lock_node_tx(&mut tx, node, now_ms).await?;
        let push_node = get_node_tx(&mut tx, node)
            .await?
            .ok_or_else(|| XmppError::item_not_found(Some("Push node not found".to_string())))?;
        if push_node.status != PushNodeStatus::Active {
            return Err(XmppError::item_not_found(Some(
                "Push node not active".to_string(),
            )));
        }
        if push_node.owner_bare_jid != *publisher {
            return Err(XmppError::forbidden(Some(
                "Only the node owner may publish Push Service notifications".to_string(),
            )));
        }
        if let Some(push_service_jid) = push_service_jid {
            ensure_active_registration_tx(&mut tx, publisher, push_service_jid.as_str(), node)
                .await?;
        }
        validate_xep0357_notification(item)?;
        if let Some(item_id) = item.id.as_deref() {
            validate_len("XEP-0060 item id", item_id, MAX_PUBSUB_ITEM_ID_LEN)?;
        }

        let item_id = item
            .id
            .clone()
            .unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
        let payload_xml = item
            .payload
            .as_ref()
            .map(String::from)
            .ok_or_else(|| XmppError::internal("validated XEP-0357 item missing payload"))?;
        let publish_options_xml = publish_options.map(String::from);
        if let Some(delivery) = canonical_delivery_id {
            let mut rows = tx.query("SELECT job_id, owner_bare_jid, node, push_service_jid, item_id, payload_xml, publish_options_xml FROM push_publish_jobs WHERE ancestry_job_id = ?", crate::db_params![delivery.to_string()]).await.map_err(|error| XmppError::internal(error.to_string()))?;
            if let Some(row) = rows
                .next()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?
            {
                let existing_id: String = row
                    .get(0)
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                let owner: String = row
                    .get(1)
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                let existing_node: String = row
                    .get(2)
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                let service = row
                    .get::<Option<String>>(3)
                    .map_err(|error| XmppError::internal(error.to_string()))?
                    .map(|raw| raw.parse::<BareJid>())
                    .transpose()
                    .map_err(|_| XmppError::internal("invalid stored push service JID"))?;
                let existing_item: String = row
                    .get(4)
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                let payload: String = row
                    .get(5)
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                let options: Option<String> = row
                    .get(6)
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                if owner != publisher.to_string()
                    || existing_node != node
                    || service.as_ref() != push_service_jid
                    || existing_item != item_id
                    || payload != payload_xml
                    || options != publish_options_xml
                {
                    return Err(XmppError::conflict(Some(
                        "canonical delivery already accepted with a different target or payload"
                            .to_string(),
                    )));
                }
                let job_id = uuid::Uuid::parse_str(&existing_id)
                    .map_err(|_| XmppError::internal("invalid canonical acceptance identity"))?;
                tx.commit()
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                return Ok(PushPublishJobEnqueue {
                    job_id,
                    item_id,
                    queued: false,
                });
            }
        }
        // Bound pending custody and the settlement tail without evicting
        // accepted work or its same-key replay evidence.
        prune_publish_jobs_tx(&mut tx, node, MAX_PUBLISH_JOBS_PER_NODE.saturating_sub(1)).await?;
        let mut quota = tx
            .query(
                "SELECT COUNT(*) FROM push_publish_jobs WHERE node = ? AND (ancestry_job_id IS NULL OR ancestry_job_id != ?)",
                crate::db_params![node, canonical_delivery_id.map(|id| id.to_string()).unwrap_or_default()],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        if let Some(row) = quota
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            if row
                .get::<i64>(0)
                .map_err(|error| XmppError::internal(error.to_string()))?
                >= MAX_PUBLISH_JOBS_PER_NODE
            {
                return Err(XmppError::Stanza {
                    condition: waddle_xmpp::StanzaErrorCondition::ResourceConstraint,
                    error_type: waddle_xmpp::StanzaErrorType::Wait,
                    text: Some("durable notification queue is full".to_string()),
                });
            }
        }
        let ancestry_job_id = canonical_delivery_id;
        let acceptance_scope = if ancestry_job_id.is_some() {
            "canonical"
        } else {
            "wire"
        };
        let mut job_id = uuid::Uuid::new_v4();
        let publication_order = allocate_publication_order_tx(&mut tx, node).await?;
        let changed = tx.execute(r#"
            INSERT INTO push_publish_jobs (
                job_id, owner_bare_jid, push_service_jid, node, item_id, payload_xml,
                publish_options_xml, ancestry_job_id, acceptance_scope, publication_order, status, attempt_count,
                last_error, next_retry_at_ms, claimed_at_ms, created_at_ms, updated_at_ms, published_at_ms
            ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, NULL, NULL, ?, ?, NULL)
            ON CONFLICT(ancestry_job_id) WHERE ancestry_job_id IS NOT NULL DO NOTHING
        "#, crate::db_params![job_id.to_string(), publisher.to_string(), push_service_jid.map(ToString::to_string), node, item_id.clone(), payload_xml.clone(), publish_options_xml.clone(), ancestry_job_id.map(|id| id.to_string()), acceptance_scope, publication_order, PUBLISH_JOB_STATUS_QUEUED, now_ms, now_ms]).await.map_err(|error| XmppError::internal(error.to_string()))?;
        if changed == 0 {
            let mut rows = tx.query("SELECT job_id, owner_bare_jid, node, push_service_jid, payload_xml, publish_options_xml FROM push_publish_jobs WHERE ancestry_job_id = ?",
                crate::db_params![ancestry_job_id.map(|id| id.to_string())]).await.map_err(|error| XmppError::internal(error.to_string()))?;
            let Some(row) = rows
                .next()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?
            else {
                return Err(XmppError::internal("canonical acceptance disappeared"));
            };
            let stored_job: String = row
                .get(0)
                .map_err(|error| XmppError::internal(error.to_string()))?;
            let stored_owner: String = row
                .get(1)
                .map_err(|error| XmppError::internal(error.to_string()))?;
            let stored_node: String = row
                .get(2)
                .map_err(|error| XmppError::internal(error.to_string()))?;
            let stored_service = row
                .get::<Option<String>>(3)
                .map_err(|error| XmppError::internal(error.to_string()))?
                .map(|raw| raw.parse::<BareJid>())
                .transpose()
                .map_err(|_| XmppError::internal("invalid stored push service JID"))?;
            let stored_payload: String = row
                .get(4)
                .map_err(|error| XmppError::internal(error.to_string()))?;
            let stored_options: Option<String> = row
                .get(5)
                .map_err(|error| XmppError::internal(error.to_string()))?;
            if stored_owner != publisher.to_string()
                || stored_node != node
                || stored_service.as_ref() != push_service_jid
                || stored_payload != payload_xml
                || stored_options != publish_options_xml
            {
                return Err(XmppError::conflict(Some(
                    "canonical delivery already accepted with a different target or payload"
                        .to_string(),
                )));
            }
            job_id = uuid::Uuid::parse_str(&stored_job)
                .map_err(|_| XmppError::internal("invalid canonical scheduler acceptance"))?;
        }
        prune_publish_jobs_tx(&mut tx, node, MAX_PUBLISH_JOBS_PER_NODE).await?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;

        Ok(PushPublishJobEnqueue {
            job_id,
            item_id,
            queued: changed > 0,
        })
    }

    pub(super) async fn recover_stale_publish_job_claims(&self) -> Result<(), XmppError> {
        let now_ms = crate::time::now_ms();
        let retry_at_ms = retry_at_ms(now_ms);
        // Clear the `claim_token` as part of recovery: a new claim
        // will mint a fresh token, and the original worker's stale
        // token can no longer match in phase 3's gating UPDATE — so
        // even if the original phase 2 eventually completes its HTTP
        // round-trip, its completion writes are fenced. An unknown provider
        // send can still be delivered twice when the recovered job retries.
        self.execute(
            r#"
            UPDATE push_publish_jobs
            SET status = ?,
                last_error = ?,
                next_retry_at_ms = ?,
                claimed_at_ms = NULL,
                claim_token = NULL,
                uncertain_send = 1,
                updated_at_ms = ?
            WHERE status = ?
              AND claimed_at_ms IS NOT NULL
              AND claimed_at_ms <= ?
            "#,
            crate::db_params![
                PUBLISH_JOB_STATUS_QUEUED,
                "Push publish job claim expired before completion",
                retry_at_ms,
                now_ms,
                PUBLISH_JOB_STATUS_IN_PROGRESS,
                now_ms - PUBLISH_JOB_CLAIM_TIMEOUT_MS,
            ],
        )
        .await?;
        Ok(())
    }

    pub(super) async fn recover_stale_publish_job_claim_by_id(
        &self,
        job_id: &str,
    ) -> Result<(), XmppError> {
        let now_ms = crate::time::now_ms();
        self.execute(
            r#"
            UPDATE push_publish_jobs
            SET status = ?,
                last_error = ?,
                next_retry_at_ms = NULL,
                claimed_at_ms = NULL,
                claim_token = NULL,
                uncertain_send = 1,
                updated_at_ms = ?
            WHERE job_id = ?
              AND status = ?
              AND claimed_at_ms IS NOT NULL
              AND claimed_at_ms <= ?
            "#,
            crate::db_params![
                PUBLISH_JOB_STATUS_QUEUED,
                "Push publish job claim expired before direct publish retry",
                now_ms,
                job_id,
                PUBLISH_JOB_STATUS_IN_PROGRESS,
                now_ms - PUBLISH_JOB_CLAIM_TIMEOUT_MS,
            ],
        )
        .await?;
        Ok(())
    }

    /// Bound known pre-dispatch failures without claiming a send occurred.
    /// An outer processing error carries no claim capability: in-progress and
    /// previously uncertain sends retain their leases or retryable custody.
    pub(super) async fn record_publish_job_failure_by_id(
        &self,
        job_id: &str,
        error: &str,
    ) -> Result<(), XmppError> {
        // Ordinary repair must not reacquire the contended canonical parent
        // that may have caused processing to fail. Only terminal disposition
        // requires ancestry locks; this atomic update excludes that case.
        const REPAIR_SQL: &str = r#"
            UPDATE push_publish_jobs
            SET uncertain_send = CASE WHEN status = 'in-progress' THEN 1 ELSE uncertain_send END,
                attempt_count = CASE WHEN status = 'queued' THEN attempt_count + 1 ELSE attempt_count END,
                last_error = CASE WHEN status = 'queued' THEN ? ELSE last_error END,
                next_retry_at_ms = CASE WHEN status = 'queued' THEN ? ELSE next_retry_at_ms END,
                updated_at_ms = CASE WHEN status = 'queued' THEN ? ELSE updated_at_ms END
            WHERE job_id = ? AND status IN (?, ?)
              AND (? = 1 OR status = 'in-progress' OR uncertain_send = 1 OR attempt_count < ?)
        "#;
        let now_ms = crate::time::now_ms();
        let retry_at = retry_at_ms(now_ms);
        let repaired = self
            .execute(
                REPAIR_SQL,
                crate::db_params![
                    error,
                    retry_at,
                    now_ms,
                    job_id,
                    PUBLISH_JOB_STATUS_QUEUED,
                    PUBLISH_JOB_STATUS_IN_PROGRESS,
                    0_i64,
                    PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS - 1
                ],
            )
            .await?;
        if repaired > 0 {
            return Ok(());
        }
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let Some(lock_target) = get_publish_job_tx(&mut tx, job_id).await? else {
            return Ok(());
        };
        match lock_notification_ancestry_typed_tx(&mut tx, &lock_target).await {
            Ok(()) => {}
            Err(NotificationAncestryLockError::Contended) => {
                // Release every partial lock before scheduling another retry.
                // Contention defers the cap; it cannot discharge custody or
                // prevent the drain from processing unrelated queued jobs.
                tx.rollback()
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                self.execute(
                    REPAIR_SQL,
                    crate::db_params![
                        error,
                        retry_at,
                        now_ms,
                        job_id,
                        PUBLISH_JOB_STATUS_QUEUED,
                        PUBLISH_JOB_STATUS_IN_PROGRESS,
                        1_i64,
                        PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS - 1
                    ],
                )
                .await?;
                return Ok(());
            }
            Err(NotificationAncestryLockError::Failed(error)) => return Err(error),
        }
        lock_owner_tx(&mut tx, lock_target.owner_bare_jid(), now_ms).await?;
        lock_node_tx(&mut tx, lock_target.node(), now_ms).await?;
        let Some(job) = get_publish_job_tx(&mut tx, job_id).await? else {
            return Ok(());
        };
        let capped = job.status() == PUBLISH_JOB_STATUS_QUEUED
            && !job.uncertain_send
            && read_publish_job_attempt_count_tx(&mut tx, job_id)
                .await?
                .unwrap_or(0)
                >= PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS - 1;
        if capped {
            let changed = tx.execute(
                r#"
                UPDATE push_publish_jobs
                SET status = ?,
                    attempt_count = attempt_count + 1,
                    last_error = ?,
                    next_retry_at_ms = NULL,
                    claimed_at_ms = NULL,
                    claim_token = NULL,
                    updated_at_ms = ?
                WHERE job_id = ? AND status = ? AND uncertain_send = 0
                "#,
                crate::db_params![
                    PUBLISH_JOB_STATUS_FAILED,
                    format!("pre-dispatch retry cap exceeded ({PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS}); last: {error}"),
                    now_ms,
                    job_id,
                    PUBLISH_JOB_STATUS_QUEUED,
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
            if changed > 0 {
                settle_terminal_notification_ancestry_tx(&mut tx, &job).await?;
                prune_delivery_attempts_tx(&mut tx, job.node(), MAX_DELIVERY_ATTEMPTS_PER_NODE)
                    .await?;
                prune_publish_jobs_tx(&mut tx, job.node(), MAX_PUBLISH_JOBS_PER_NODE).await?;
            }
        } else {
            tx.execute(
                REPAIR_SQL,
                crate::db_params![
                    error,
                    retry_at,
                    now_ms,
                    job_id,
                    PUBLISH_JOB_STATUS_QUEUED,
                    PUBLISH_JOB_STATUS_IN_PROGRESS,
                    1_i64,
                    PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS - 1
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        }
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))
    }

    pub async fn queued_publish_jobs(&self) -> Result<Vec<PushPublishJob>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT job_id, owner_bare_jid, node, item_id, push_service_jid, status, claim_token, ancestry_job_id, acceptance_scope, publication_order, backing_state, uncertain_send
                FROM push_publish_jobs
                WHERE status = ?
                ORDER BY created_at_ms ASC, job_id ASC
                "#,
                crate::db_params![PUBLISH_JOB_STATUS_QUEUED],
            )
            .await?;
        let mut jobs = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            jobs.push(decode_publish_job(&row)?);
        }
        Ok(jobs)
    }

    pub async fn delivery_attempts_for_node(
        &self,
        node: &str,
    ) -> Result<Vec<PushDeliveryAttempt>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT attempt_id, node, device_id, item_id, status
                FROM push_delivery_attempts
                WHERE node = ?
                ORDER BY created_at_ms ASC, attempt_id ASC
                "#,
                crate::db_params![node],
            )
            .await?;
        let mut attempts = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            attempts.push(decode_attempt(&row)?);
        }
        Ok(attempts)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::push_service::dispatch;
    use crate::push_service::test_support::{notification_item, owner, scalar_i64, store};
    use crate::push_service::{PushDevicePlatform, PushDeviceRegistration};

    // #1126: the requeue delay carries ±25% jitter so a relay outage
    // does not produce synchronized retry waves on the 60s beat.
    #[test]
    fn retry_at_ms_is_jittered_within_bounds() {
        let now_ms = 1_000_000;
        let mut seen = std::collections::HashSet::new();
        for _ in 0..200 {
            let retry_at = retry_at_ms(now_ms);
            let delay = retry_at - now_ms;
            assert!(
                (45_000..=75_000).contains(&delay),
                "jittered delay {delay} outside ±25% of {PUBLISH_JOB_RETRY_DELAY_MS}"
            );
            seen.insert(delay);
        }
        assert!(
            seen.len() > 1,
            "200 samples produced a single delay — backoff is not jittered"
        );
    }

    #[tokio::test]
    async fn failure_repair_without_claim_proof_preserves_successor_lease() {
        let store = store().await;
        let owner = owner();
        let node = store
            .ensure_node(&owner, "repair-ownership")
            .await
            .expect("node");
        let accepted = store
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("repair-ownership"),
                &owner,
            )
            .await
            .expect("acceptance");
        let successor_at = crate::time::now_ms();
        let successor = uuid::Uuid::new_v4().to_string();
        store.execute("UPDATE push_publish_jobs SET status = 'in-progress', claim_token = ?, claimed_at_ms = ?, attempt_count = 50 WHERE job_id = ?", crate::db_params![successor.clone(), successor_at, accepted.job_id().to_string()]).await.expect("successor claim");
        store
            .record_publish_job_failure_by_id(
                &accepted.job_id().to_string(),
                "late predecessor error",
            )
            .await
            .expect("repair");
        let repaired = store
            .load_publish_job(&accepted.job_id().to_string())
            .await
            .expect("load")
            .expect("job");
        assert_eq!(repaired.status(), PUBLISH_JOB_STATUS_IN_PROGRESS);
        assert_eq!(repaired.claim_token(), successor);
        assert!(repaired.uncertain_send);
        let mut rows = store
            .query(
                "SELECT claimed_at_ms, attempt_count FROM push_publish_jobs WHERE job_id = ?",
                crate::db_params![accepted.job_id().to_string()],
            )
            .await
            .expect("lease");
        let row = rows.next().await.expect("row").expect("claim");
        assert_eq!(row.get::<i64>(0).expect("claimed time"), successor_at);
        assert_eq!(row.get::<i64>(1).expect("attempts"), 50);
    }

    #[tokio::test]
    async fn publish_job_claim_is_exclusive_after_first_claim_commits() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        store
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("exclusive-claim"),
                &owner,
            )
            .await
            .expect("enqueue");
        let job_id = store.queued_publish_jobs().await.expect("queued jobs")[0]
            .job_id()
            .to_string();

        let now_ms = crate::time::now_ms();
        let mut first_tx = store.db.begin().await.expect("first tx");
        assert!(claim_publish_job_tx(&mut first_tx, &job_id, now_ms)
            .await
            .expect("first claim")
            .is_some());
        first_tx.commit().await.expect("first commit");

        let mut second_tx = store.db.begin().await.expect("second tx");
        assert!(claim_publish_job_tx(&mut second_tx, &job_id, now_ms + 1)
            .await
            .expect("second claim")
            .is_none());
        second_tx.commit().await.expect("second commit");

        assert_eq!(
            scalar_i64(
                &store,
                "SELECT COUNT(*) FROM push_publish_jobs WHERE status = ?",
                crate::db_params![PUBLISH_JOB_STATUS_IN_PROGRESS],
            )
            .await,
            1
        );
    }

    #[tokio::test]
    async fn publish_job_pruning_preserves_unresolved_acceptance_per_node() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        for item_id in ["queued-1", "queued-2", "queued-3"] {
            tokio::time::sleep(std::time::Duration::from_millis(2)).await;
            store
                .enqueue_notification_publish_job_from_user_server(
                    node.node(),
                    &notification_item(item_id),
                    &owner,
                )
                .await
                .expect("enqueue");
        }

        let mut tx = store.db.begin().await.expect("tx");
        prune_publish_jobs_tx(&mut tx, node.node(), 2)
            .await
            .expect("prune jobs");
        tx.commit().await.expect("commit");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");
        let item_ids = queued
            .iter()
            .map(|job| job.item_id().to_string())
            .collect::<Vec<_>>();

        assert_eq!(
            item_ids,
            vec![
                "queued-1".to_string(),
                "queued-2".to_string(),
                "queued-3".to_string()
            ]
        );
    }

    #[tokio::test]
    async fn delivery_attempt_pruning_keeps_newest_attempts_per_node() {
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
        for idx in 0..5 {
            store
                .execute(
                    r#"
                    INSERT INTO push_delivery_attempts (
                        attempt_id,
                        node,
                        device_id,
                        platform,
                        item_id,
                        status,
                        last_error,
                        created_at_ms
                    ) VALUES (?, ?, ?, ?, ?, ?, NULL, ?)
                    "#,
                    crate::db_params![
                        format!("attempt-{idx}"),
                        node.node(),
                        "web-1",
                        PushDevicePlatform::Web.to_string(),
                        format!("item-{idx}"),
                        dispatch::ATTEMPT_STATUS_FAKE_SENT_NON_WEB,
                        idx as i64,
                    ],
                )
                .await
                .expect("attempt row");
        }

        let db = store.database();
        let mut tx = db.begin().await.expect("transaction");
        prune_delivery_attempts_tx(&mut tx, node.node(), 3)
            .await
            .expect("prune attempts");
        tx.commit().await.expect("commit prune");

        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        let item_ids = attempts
            .iter()
            .map(|attempt| attempt.item_id())
            .collect::<Vec<_>>();

        assert_eq!(item_ids, vec!["item-2", "item-3", "item-4"]);
    }

    // #1123 (Greptile review): retention pruning must not evict the
    // delivered-attempt record of a job that is still retryable — the
    // per-device idempotency filter reads it on the next retry, and
    // losing it would re-push the item to an already-delivered device.
    #[tokio::test]
    async fn delivery_attempt_pruning_exempts_still_retryable_jobs() {
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
        // A QUEUED (retryable) publish job for the oldest item.
        store
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("retrying-item"),
                &owner,
            )
            .await
            .expect("enqueue retryable job");
        let retry_job = store.queued_publish_jobs().await.expect("job")[0]
            .job_id()
            .to_string();
        // Oldest attempt belongs to the retryable job; the rest are
        // newer attempts for terminal (no-job) items.
        for (idx, item_id) in ["retrying-item", "done-1", "done-2", "done-3", "done-4"]
            .iter()
            .enumerate()
        {
            store
                .execute(
                    r#"
                    INSERT INTO push_delivery_attempts (
                        publish_job_id,
                        attempt_id,
                        node,
                        device_id,
                        platform,
                        item_id,
                        status,
                        last_error,
                        created_at_ms
                    ) VALUES (?, ?, ?, ?, ?, ?, ?, NULL, ?)
                    "#,
                    crate::db_params![
                        if *item_id == "retrying-item" {
                            Some(retry_job.clone())
                        } else {
                            None
                        },
                        format!("attempt-{idx}"),
                        node.node(),
                        "web-1",
                        PushDevicePlatform::Web.to_string(),
                        item_id.to_string(),
                        dispatch::ATTEMPT_STATUS_WEB_DELIVERED,
                        idx as i64,
                    ],
                )
                .await
                .expect("attempt row");
        }

        let db = store.database();
        let mut tx = db.begin().await.expect("transaction");
        prune_delivery_attempts_tx(&mut tx, node.node(), 2)
            .await
            .expect("prune attempts");
        tx.commit().await.expect("commit prune");

        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        let item_ids = attempts
            .iter()
            .map(|attempt| attempt.item_id())
            .collect::<Vec<_>>();

        assert!(
            item_ids.contains(&"retrying-item"),
            "the retryable job's delivered attempt must survive pruning, got {item_ids:?}"
        );
        assert!(
            item_ids.contains(&"done-3") && item_ids.contains(&"done-4"),
            "the newest terminal attempts stay within the retention tail"
        );
        assert!(
            !item_ids.contains(&"done-1"),
            "terminal items past the tail still prune"
        );
    }
}
