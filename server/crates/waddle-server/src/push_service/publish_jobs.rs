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
use super::types::{PushDeliveryAttempt, PushNodeStatus, PushPublishJob, PushPublishJobEnqueue};

pub(super) const PUBLISH_JOB_STATUS_QUEUED: &str = "queued";

pub(super) const PUBLISH_JOB_STATUS_IN_PROGRESS: &str = "in-progress";

pub(super) const PUBLISH_JOB_STATUS_PUBLISHED: &str = "published";

pub(super) const PUBLISH_JOB_STATUS_FAILED: &str = "failed";

pub(super) const PUBLISH_JOB_ERROR_NO_ACTIVE_DEVICES: &str = "Push node has no active devices";

pub(super) const MAX_DELIVERY_ATTEMPTS_PER_NODE: i64 = 10_000;

pub(super) const MAX_PUBLISH_JOBS_PER_NODE: i64 = 10_000;

pub(super) const MAX_PUBSUB_ITEM_ID_LEN: usize = 256;

pub(super) const PUBLISH_JOB_RETRY_DELAY_MS: i64 = 60_000;

/// Upper bound on the duration of one publish-job worker pass (phase 1
/// claim → phase 2 HTTP fan-out → phase 3 record). Sized to comfortably
/// exceed any realistic phase-2 elapsed time: 1000 same-relay devices ×
/// 100ms per-bucket spacing = ~100s of best-case throughput, well below
/// the cap. `recover_stale_publish_job_claims` resets claims older than
/// this; setting it lower than realistic phase-2 duration risks a
/// concurrent worker re-claiming and dispatching the same job in
/// parallel. Until a claim-token UUID column lands (see
/// `TODO(#762 follow-up)` in `finalize_publish_job`), this constant
/// is the only mitigation for the at-most-once invariant.
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
            SELECT job_id, owner_bare_jid, node, item_id, push_service_jid, status, claim_token
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
/// this `(node, item_id)` — `web-delivered` for real Web Push sends,
/// `fake-sent` for the stubbed APNS/FCM platforms. A retried publish
/// job filters its fan-out against this set so one transiently
/// failing sibling does not turn into duplicate OS notifications on
/// every device that already received the item (#1123).
pub(super) async fn delivered_device_ids_for_item_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
    item_id: &str,
) -> Result<std::collections::HashSet<String>, XmppError> {
    let mut rows = tx
        .query(
            r#"
            SELECT DISTINCT device_id
            FROM push_delivery_attempts
            WHERE node = ? AND item_id = ? AND status IN (?, ?)
            "#,
            crate::db_params![
                node,
                item_id,
                super::dispatch::ATTEMPT_STATUS_WEB_DELIVERED,
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
    Ok(())
}

pub(super) async fn prune_delivery_attempts_tx(
    tx: &mut crate::db::Transaction<'_>,
    node: &str,
    limit: i64,
) -> Result<(), XmppError> {
    // Terminal-success attempts of a still-retryable job are exempt
    // from the retention tail (#1123, Greptile review): the per-device
    // idempotency filter reads `web-delivered`/`fake-sent` rows for
    // the job's `(node, item_id)` on every retry, so evicting one
    // mid-retry would re-push the item to a device that already
    // received it. Only that narrow slice is protected — failure/
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
              status IN (?, ?)
              AND item_id IN (
                  SELECT item_id
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
    tx.execute(
        r#"
        DELETE FROM push_publish_jobs
        WHERE node = ?
          AND status != ?
          AND job_id NOT IN (
              SELECT job_id
              FROM push_publish_jobs
              WHERE node = ?
              ORDER BY created_at_ms DESC, job_id DESC
              LIMIT ?
        )
        "#,
        crate::db_params![node, PUBLISH_JOB_STATUS_IN_PROGRESS, node, limit,],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
    Ok(())
}

pub(super) async fn delete_retryable_publish_jobs_for_node_tx(
    tx: &mut crate::db::Transaction<'_>,
    owner_bare_jid: &BareJid,
    node: &str,
) -> Result<(), XmppError> {
    tx.execute(
        r#"
        DELETE FROM push_publish_jobs
        WHERE owner_bare_jid = ?
          AND node = ?
          AND status IN (?, ?)
        "#,
        crate::db_params![
            owner_bare_jid.to_string(),
            node,
            PUBLISH_JOB_STATUS_QUEUED,
            PUBLISH_JOB_STATUS_IN_PROGRESS,
        ],
    )
    .await
    .map_err(|error| XmppError::internal(error.to_string()))?;
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
            .get(4)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        status: row
            .get(5)
            .map_err(|error| XmppError::internal(error.to_string()))?,
        claim_token: claim_token.unwrap_or_default(),
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
        self.enqueue_notification_publish_job(
            node,
            item,
            publisher,
            push_service_jid,
            publish_options,
        )
        .await
    }

    async fn enqueue_notification_publish_job(
        &self,
        node: &str,
        item: &PubSubItem,
        publisher: &BareJid,
        push_service_jid: Option<&str>,
        publish_options: Option<&Element>,
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
            ensure_active_registration_tx(&mut tx, publisher, push_service_jid, node).await?;
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
        let job_id = uuid::Uuid::new_v4().to_string();
        let changed = tx
            .execute(
                r#"
                INSERT INTO push_publish_jobs (
                    job_id,
                    owner_bare_jid,
                    push_service_jid,
                    node,
                    item_id,
                    payload_xml,
                    publish_options_xml,
                    status,
                    attempt_count,
                    last_error,
                    next_retry_at_ms,
                    claimed_at_ms,
                    created_at_ms,
                    updated_at_ms,
                    published_at_ms
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, 0, NULL, NULL, NULL, ?, ?, NULL)
                ON CONFLICT(node, item_id) DO UPDATE SET
                    push_service_jid = excluded.push_service_jid,
                    payload_xml = excluded.payload_xml,
                    publish_options_xml = excluded.publish_options_xml,
                    status = ?,
                    last_error = NULL,
                    next_retry_at_ms = NULL,
                    claimed_at_ms = NULL,
                    updated_at_ms = excluded.updated_at_ms,
                    published_at_ms = NULL
                WHERE push_publish_jobs.status IN (?, ?)
                "#,
                crate::db_params![
                    job_id,
                    publisher.to_string(),
                    push_service_jid,
                    node,
                    item_id.clone(),
                    payload_xml,
                    publish_options_xml.clone(),
                    PUBLISH_JOB_STATUS_QUEUED,
                    now_ms,
                    now_ms,
                    PUBLISH_JOB_STATUS_QUEUED,
                    PUBLISH_JOB_STATUS_QUEUED,
                    PUBLISH_JOB_STATUS_FAILED,
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        prune_publish_jobs_tx(&mut tx, node, MAX_PUBLISH_JOBS_PER_NODE).await?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;

        Ok(PushPublishJobEnqueue {
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
        // round-trip, its attempt-writes and job-state transition
        // will be silently dropped instead of double-delivering.
        self.execute(
            r#"
            UPDATE push_publish_jobs
            SET status = ?,
                last_error = ?,
                next_retry_at_ms = ?,
                claimed_at_ms = NULL,
                claim_token = NULL,
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

    pub(super) async fn publish_job_id_for_node_item(
        &self,
        node: &str,
        item_id: &str,
    ) -> Result<Option<String>, XmppError> {
        let mut rows = self
            .query(
                "SELECT job_id FROM push_publish_jobs WHERE node = ? AND item_id = ?",
                crate::db_params![node, item_id],
            )
            .await?;
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

    pub(super) async fn record_publish_job_failure(
        &self,
        node: &str,
        item_id: &str,
        error: &str,
    ) -> Result<(), XmppError> {
        let Some(job_id) = self.publish_job_id_for_node_item(node, item_id).await? else {
            return Ok(());
        };
        self.record_publish_job_failure_by_id(&job_id, error).await
    }

    pub(super) async fn record_publish_job_failure_by_id(
        &self,
        job_id: &str,
        error: &str,
    ) -> Result<(), XmppError> {
        let now_ms = crate::time::now_ms();
        let next_retry_at_ms = retry_at_ms(now_ms);
        self.execute(
            r#"
            UPDATE push_publish_jobs
            SET status = ?,
                attempt_count = attempt_count + 1,
                last_error = ?,
                next_retry_at_ms = ?,
                claimed_at_ms = NULL,
                updated_at_ms = ?
            WHERE job_id = ? AND status IN (?, ?)
            "#,
            crate::db_params![
                PUBLISH_JOB_STATUS_QUEUED,
                error,
                next_retry_at_ms,
                now_ms,
                job_id,
                PUBLISH_JOB_STATUS_QUEUED,
                PUBLISH_JOB_STATUS_IN_PROGRESS,
            ],
        )
        .await?;
        Ok(())
    }

    pub async fn queued_publish_jobs(&self) -> Result<Vec<PushPublishJob>, XmppError> {
        let mut rows = self
            .query(
                r#"
                SELECT job_id, owner_bare_jid, node, item_id, push_service_jid, status, claim_token
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
    async fn publish_job_pruning_bounds_old_queued_jobs_per_node() {
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
            vec!["queued-2".to_string(), "queued-3".to_string()]
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
