//! Publish-job worker: the three-phase claim/dispatch/finalize pipeline
//! that fans a queued XEP-0357 notification out to registered devices,
//! plus attempt-status classification and XEP-0357 §6 device cleanup.

use std::sync::Arc;

use jid::BareJid;
use waddle_xmpp::push::types::VapidSub;
use waddle_xmpp::push::vapid::VapidSigner;
use waddle_xmpp::push::WebPushSender;
use waddle_xmpp::telemetry::attributes::MetricAttribute;
use waddle_xmpp::XmppError;

use super::devices::{
    active_devices_with_subscription_for_node_tx, count_active_devices_for_node_tx,
    mark_device_disabled_tx,
};
use super::dispatch;
use super::nodes::get_node_tx;
use super::publish_jobs::{
    claim_publish_job_tx, delivered_device_ids_for_item_tx, get_publish_job_payload_xml_tx,
    get_publish_job_tx, mark_publish_job_failed_tx, prune_delivery_attempts_tx,
    prune_publish_jobs_tx, read_publish_job_attempt_count_tx, read_publish_job_claim_token_tx,
    retry_at_ms, MAX_DELIVERY_ATTEMPTS_PER_NODE, MAX_PUBLISH_JOBS_PER_NODE,
    PUBLISH_JOB_ERROR_NO_ACTIVE_DEVICES, PUBLISH_JOB_MAX_RETRY_AFTER_MS,
    PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS, PUBLISH_JOB_STATUS_FAILED, PUBLISH_JOB_STATUS_IN_PROGRESS,
    PUBLISH_JOB_STATUS_PUBLISHED, PUBLISH_JOB_STATUS_QUEUED,
};
use super::registration::ensure_active_registration_tx;
use super::secrets::PushSecretCipher;
use super::store::{lock_node_tx, lock_owner_tx, DatabasePushServiceStore};
use super::types::{PushDevicePlatform, PushFanoutResult, PushNodeStatus, PushPublishJob};

/// Capability-degraded marker: the publish-job worker fired but the
/// process is missing the VAPID signer, Web Push transport, or `sub`
/// claim. Treated as transient so the next worker tick will retry once
/// the operator wires up the missing piece.
const ATTEMPT_STATUS_WEB_NOT_CONFIGURED: &str = "web-not-configured";

/// Internal error during Web Push dispatch (encrypt/sign/aud derivation
/// failure). Classified PERMANENT, not transient: encrypt/sign/aud
/// failures are deterministic on the inputs (same subscription, same
/// payload, same signer key), so retrying produces the same error
/// indefinitely. The job is marked PUBLISHED with the failure visible
/// in `push_delivery_attempts.last_error` so an operator sees the bug
/// and acts; the next *new* publish job tries the fixed code path.
/// See `attempt_status_is_transient` — this constant is intentionally
/// NOT in the transient set.
const ATTEMPT_STATUS_WEB_INTERNAL_ERROR: &str = "web-internal-error";

/// Maximum number of characters persisted in
/// `push_delivery_attempts.last_error`. The column is `TEXT`, but we
/// keep diagnostics short so a runaway provider body cannot bloat the
/// table.
const PUSH_ATTEMPT_LAST_ERROR_MAX_CHARS: usize = 200;

/// Per-device outcome the publish-job worker carries from phase 2
/// (out-of-tx Web Push dispatch) back into phase 3 (the
/// `push_delivery_attempts` insert + job finalize). Keeping it typed
/// here means the dispatcher does not touch the DB and the writer does
/// not touch the network.
#[derive(Debug, Clone)]
struct DispatchedAttempt {
    device_id: String,
    platform: PushDevicePlatform,
    status: &'static str,
    last_error: Option<String>,
    /// `Retry-After` carried by a `WebPushOutcome::RateLimited` response.
    /// Phase 3 honors the larger of this and the default backoff when
    /// requeuing, so a relay's explicit "back off N seconds" hint is not
    /// dropped on the floor.
    retry_after: Option<std::time::Duration>,
}

/// Output of phase 1 of the publish-job worker — the typed pieces
/// phase 2 needs to fan out Web Push deliveries without holding a DB
/// transaction.
struct PublishWorkPhase1 {
    job: PushPublishJob,
    sealed_devices: Vec<dispatch::SealedActiveDevice>,
    payload_xml: String,
    /// How many active devices were filtered out of this pass because
    /// an earlier pass already delivered the item to them (#1123).
    /// Finalize uses this to disable the "all devices returned an
    /// encoder-bug status" FAILED classification: with a prior
    /// success, a uniform failure among the REMAINING devices is not
    /// "all devices" and the job must still complete.
    prior_delivered_devices: usize,
}

/// Phase 1 can either continue into phase 2/3 with a [`PublishWorkPhase1`]
/// or short-circuit (claim contention, validation failure, no active
/// devices). The short-circuit variant carries the final
/// [`PushFanoutResult`] (or `None` when there was nothing to do).
enum Phase1Outcome {
    Continue(PublishWorkPhase1),
    ShortCircuit(Option<PushFanoutResult>),
}

/// Render a typed [`WebPushOutcome`] as a short diagnostic string for
/// the `push_delivery_attempts.last_error` column. Kept here next to
/// the worker so the wire shape of `last_error` matches the typed
/// outcome 1:1.
fn web_push_outcome_diagnostic(outcome: &waddle_xmpp::push::types::WebPushOutcome) -> String {
    use waddle_xmpp::push::types::{TransientFailure, WebPushOutcome};
    match outcome {
        WebPushOutcome::Delivered { status } => format!("delivered HTTP {status}"),
        WebPushOutcome::SubscriptionGone { status } => format!("subscription gone HTTP {status}"),
        WebPushOutcome::ClockSkew { status } => format!("clock skew HTTP {status}"),
        WebPushOutcome::RateLimited {
            status,
            retry_after,
        } => match retry_after {
            Some(duration) => format!(
                "rate limited HTTP {status} retry-after {}s",
                duration.as_secs()
            ),
            None => format!("rate limited HTTP {status}"),
        },
        WebPushOutcome::PayloadTooLarge { status } => format!("payload too large HTTP {status}"),
        // `status = 0` is the sentinel `HttpWebPushSender` returns for
        // local preflight failures (non-https endpoint reaching the
        // sender despite registration-time validation, or VAPID
        // header that fails `HeaderValue::from_str`) — there was no
        // HTTP exchange so no real status code applies. Render the
        // diagnostic with that context instead of the misleading
        // "HTTP 0".
        WebPushOutcome::BadRequest { status: 0 } => {
            "bad request (preflight: invalid endpoint or auth header)".to_string()
        }
        WebPushOutcome::BadRequest { status } => format!("bad request HTTP {status}"),
        WebPushOutcome::Transient { kind } => match kind {
            TransientFailure::Network => "transient: network".to_string(),
            TransientFailure::ServerError { status } => {
                format!("transient: HTTP {status}")
            }
            TransientFailure::Timeout => "transient: timeout".to_string(),
        },
    }
}

/// `true` when an attempt status represents a transient/recoverable
/// failure that should requeue the publish-job for retry. Permanent
/// statuses (delivered, gone, payload-too-large, bad-request, invalid
/// endpoint/keys, missing material, unseal failed, fake-sent) mark the
/// job as published.
/// Dispatch one Web Push device row: unseal the subscription, then
/// encrypt+sign+POST. Translates typed outcomes into the
/// `push_delivery_attempts.status` wire format. Free function (not a
/// method) so the per-device future is `Send + 'static`-compatible
/// inside the `buffer_unordered` fan-out in `dispatch_devices`.
struct WebPushDispatchProvider<'a> {
    signer: &'a Arc<dyn VapidSigner>,
    sender: &'a Arc<dyn WebPushSender>,
    sub: &'a VapidSub,
    secrets: &'a PushSecretCipher,
}

async fn dispatch_web_device_owned(
    device: &dispatch::SealedActiveDevice,
    recipient: &BareJid,
    parsed: &dispatch::ParsedPushPayload,
    item_id: &str,
    provider: WebPushDispatchProvider<'_>,
) -> DispatchedAttempt {
    let target = match dispatch::WebPushTarget::try_from_sealed(device, provider.secrets) {
        Ok(target) => target,
        Err(reason) => {
            let status = dispatch::skip_reason_to_attempt_status(reason);
            tracing::warn!(
                recipient = %recipient,
                conversation = %parsed.conversation,
                notification_class = parsed.class.as_db_value(),
                provider = "web_push",
                push_stage = "provider_dispatch_skipped",
                provider_outcome = status,
                "push provider transition"
            );
            return DispatchedAttempt {
                device_id: device.device_id.clone(),
                platform: device.platform,
                status,
                last_error: None,
                retry_after: None,
            };
        }
    };
    match dispatch::dispatch_one_web_push(
        &target,
        parsed,
        item_id,
        provider.signer,
        provider.sub,
        provider.sender,
    )
    .await
    {
        Ok(outcome) => {
            let status = dispatch::outcome_to_attempt_status(&outcome);
            match waddle_xmpp::telemetry::push_pipeline::record_web_push_outcome(&outcome) {
                Some(stage) => tracing::info!(
                    recipient = %recipient,
                    conversation = %parsed.conversation,
                    notification_class = parsed.class.as_db_value(),
                    provider = "web_push",
                    push_stage = stage.value(),
                    provider_outcome = status,
                    "push provider transition"
                ),
                None => tracing::warn!(
                    recipient = %recipient,
                    conversation = %parsed.conversation,
                    notification_class = parsed.class.as_db_value(),
                    provider = "web_push",
                    push_stage = "provider_no_response",
                    provider_outcome = status,
                    "push provider transition"
                ),
            }
            let last_error = if matches!(
                outcome,
                waddle_xmpp::push::types::WebPushOutcome::Delivered { .. }
            ) {
                None
            } else {
                Some(truncate_last_error(&web_push_outcome_diagnostic(&outcome)))
            };
            // Honor RFC 7231 §7.1.3 `Retry-After` for rate-limited
            // responses — phase 3 takes `max(default, retry_after)` so
            // a relay's "back off N seconds" hint is not lost.
            let retry_after = match &outcome {
                waddle_xmpp::push::types::WebPushOutcome::RateLimited { retry_after, .. } => {
                    *retry_after
                }
                _ => None,
            };
            // ClockSkew (401 with WWW-Authenticate: vapid) means the
            // relay rejected our JWT — likely because its clock
            // disagrees with ours past the RFC 8292 §2 `exp` skew
            // tolerance. The cached JWT is now poison: a retry that
            // re-serves the same JWT will fail identically, exhausting
            // the §6.1 attempt cap without ever sending a fresh one.
            // Invalidate the cache so the next attempt mints a new JWT
            // with a fresh `iat`/`exp` window.
            if matches!(
                outcome,
                waddle_xmpp::push::types::WebPushOutcome::ClockSkew { .. }
            ) {
                provider.signer.invalidate_cache();
            }
            // XEP-0357 §6 forward cleanup runs in
            // `finalize_publish_job` — when the persisted status hits
            // `attempt_status_warrants_device_disable`, the matching
            // `push_devices` row is flipped to `disabled` in the same
            // tx.
            //
            // TODO(#762 follow-up): emit a server-initiated
            // `<message><device-disabled xmlns='urn:waddle:push-
            // service:0'/></message>` to the registering chat client
            // so it can drop its local subscription and resubscribe
            // with a fresh device-id. Requires plumbing the connection
            // router into the publish-job worker; deferred so PR-D2
            // stays scoped to the server-side cleanup half of §6.
            DispatchedAttempt {
                device_id: device.device_id.clone(),
                platform: device.platform,
                status,
                last_error,
                retry_after,
            }
        }
        // Internal-error path covers encrypt/sign/aud-derive failures.
        // These are deterministic bugs — a retry on the same
        // `(subscription, payload)` will fail identically — so we
        // classify as PERMANENT (not transient) to avoid an unbounded
        // retry loop. Recorded as `web-internal-error` so an operator
        // can grep for it and fix the underlying bug; the device stays
        // active (not a per-device problem).
        Err(error) => {
            tracing::error!(
                recipient = %recipient,
                conversation = %parsed.conversation,
                notification_class = parsed.class.as_db_value(),
                provider = "web_push",
                push_stage = "provider_dispatch_failed",
                provider_outcome = ATTEMPT_STATUS_WEB_INTERNAL_ERROR,
                "push provider transition"
            );
            DispatchedAttempt {
                device_id: device.device_id.clone(),
                platform: device.platform,
                status: ATTEMPT_STATUS_WEB_INTERNAL_ERROR,
                last_error: Some(truncate_last_error(&error.to_string())),
                retry_after: None,
            }
        }
    }
}

fn attempt_status_is_transient(status: &str) -> bool {
    matches!(
        status,
        dispatch::ATTEMPT_STATUS_WEB_TRANSIENT
            | dispatch::ATTEMPT_STATUS_WEB_RATE_LIMITED
            | dispatch::ATTEMPT_STATUS_WEB_CLOCK_SKEW
            | ATTEMPT_STATUS_WEB_NOT_CONFIGURED
    )
    // `web-internal-error` is deliberately NOT transient: encrypt /
    // sign / aud-derive failures are deterministic bugs that recur
    // identically on retry. Classifying them as permanent means an
    // operator sees the failure in `push_delivery_attempts` and can
    // act, instead of the job spinning in a 60s loop forever.
}

/// XEP-0357 §6: which attempt outcomes mean the underlying device row
/// should be marked disabled in `push_devices` so future publish jobs
/// stop fanning out to it.
///
/// - `web-gone` is the textbook §6 trigger: the relay returned 404/410
///   meaning the subscription is permanently dead.
/// - `web-invalid-keys` / `web-invalid-endpoint` mean the stored
///   material is structurally unusable; retrying the same row will keep
///   failing with the same error.
/// - `web-unseal-failed` is deliberately NOT listed — it usually
///   signals a root-key drift across boots, which is fixable without
///   the user re-registering their device. Disabling on that would
///   compound a configuration mistake into data loss.
/// - Transient statuses are handled by the retry path; we keep the
///   device row active so the next attempt has somewhere to land.
fn attempt_status_warrants_device_disable(status: &str) -> bool {
    matches!(
        status,
        dispatch::ATTEMPT_STATUS_WEB_GONE
            | dispatch::ATTEMPT_STATUS_WEB_INVALID_KEYS
            | dispatch::ATTEMPT_STATUS_WEB_INVALID_ENDPOINT
    )
}

/// If every attempt in the fan-out carries the same encoder/config-bug
/// status (i.e. all `web-bad-request` or all `web-payload-too-large`),
/// return that status so the worker can finalize the job as FAILED
/// instead of silently marking it PUBLISHED. Returns `None` otherwise.
///
/// Both statuses are deterministic on the fan-out: every device hits
/// the same encoder/padding bug, so the job will recur identically on
/// retry. Surfacing FAILED on `push_publish_jobs.status` lets monitors
/// alert without auditing the attempts table.
fn all_attempts_with_encoder_bug_signature(attempts: &[DispatchedAttempt]) -> Option<&'static str> {
    if attempts.is_empty() {
        return None;
    }
    let first = attempts[0].status;
    let uniform = matches!(
        first,
        dispatch::ATTEMPT_STATUS_WEB_BAD_REQUEST | dispatch::ATTEMPT_STATUS_WEB_PAYLOAD_TOO_LARGE
    ) && attempts.iter().all(|a| a.status == first);
    if uniform {
        Some(first)
    } else {
        None
    }
}

/// Truncate a free-form diagnostic to fit the
/// `push_delivery_attempts.last_error` column without bloating the table
/// when a relay echoes back a multi-KB error body. Splits on a char
/// boundary so we never store half a UTF-8 sequence.
fn truncate_last_error(message: &str) -> String {
    if message.chars().count() <= PUSH_ATTEMPT_LAST_ERROR_MAX_CHARS {
        return message.to_string();
    }
    let mut out = String::with_capacity(PUSH_ATTEMPT_LAST_ERROR_MAX_CHARS);
    for (idx, ch) in message.chars().enumerate() {
        if idx >= PUSH_ATTEMPT_LAST_ERROR_MAX_CHARS {
            break;
        }
        out.push(ch);
    }
    out
}

impl DatabasePushServiceStore {
    pub async fn drain_queued_notification_publish_jobs(
        &self,
        limit: usize,
    ) -> Result<Vec<PushFanoutResult>, XmppError> {
        self.drain_queued_notification_publish_jobs_with_retention_limit(
            limit,
            MAX_DELIVERY_ATTEMPTS_PER_NODE,
        )
        .await
    }

    async fn drain_queued_notification_publish_jobs_with_retention_limit(
        &self,
        limit: usize,
        retention_limit: i64,
    ) -> Result<Vec<PushFanoutResult>, XmppError> {
        self.recover_stale_publish_job_claims().await?;
        let now_ms = crate::time::now_ms();
        let mut rows = self
            .query(
                r#"
                SELECT job_id
                FROM push_publish_jobs
                WHERE status = ?
                  AND (next_retry_at_ms IS NULL OR next_retry_at_ms <= ?)
                ORDER BY created_at_ms ASC, job_id ASC
                LIMIT ?
                "#,
                crate::db_params![PUBLISH_JOB_STATUS_QUEUED, now_ms, limit as i64],
            )
            .await?;
        let mut job_ids = Vec::new();
        while let Some(row) = rows
            .next()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?
        {
            job_ids.push(
                row.get::<String>(0)
                    .map_err(|error| XmppError::internal(error.to_string()))?,
            );
        }

        let mut results = Vec::new();
        for job_id in job_ids {
            match self
                .process_publish_job_with_retention_limit(&job_id, retention_limit)
                .await
            {
                Ok(Some(result)) => results.push(result),
                Ok(None) => {}
                Err(error) => {
                    self.record_publish_job_failure_by_id(&job_id, &error.to_string())
                        .await?;
                }
            }
        }
        Ok(results)
    }

    pub(super) async fn process_publish_job_by_node_item_with_retention_limit(
        &self,
        node: &str,
        item_id: &str,
        retention_limit: i64,
    ) -> Result<Option<PushFanoutResult>, XmppError> {
        let Some(job_id) = self.publish_job_id_for_node_item(node, item_id).await? else {
            return Ok(None);
        };
        self.recover_stale_publish_job_claim_by_id(&job_id).await?;
        self.process_publish_job_with_retention_limit(&job_id, retention_limit)
            .await
    }

    async fn process_publish_job_with_retention_limit(
        &self,
        job_id: &str,
        retention_limit: i64,
    ) -> Result<Option<PushFanoutResult>, XmppError> {
        let now_ms = crate::time::now_ms();

        // ---- Phase 1: tx1 — claim + validate + load sealed devices +
        // read payload_xml. We commit with the job still in
        // `in-progress` so its `PUBLISH_JOB_CLAIM_TIMEOUT_MS` claim
        // window covers phases 2+3 without holding a DB transaction
        // across the network round-trip.
        let phase1 = match self.process_publish_phase1(job_id, now_ms).await? {
            Phase1Outcome::Continue(state) => state,
            Phase1Outcome::ShortCircuit(result) => return Ok(result),
        };
        let PublishWorkPhase1 {
            job,
            sealed_devices,
            payload_xml,
            prior_delivered_devices,
        } = phase1;

        // ---- Phase 2: outside any tx — encrypt, sign, and send.
        // The XEP-0357 payload only needs to be parsed when we have a
        // real Web Push provider wired up. Without one, every device
        // records the legacy `fake-sent` marker and we never look at
        // the conversation / class / message-count fields. Parsing
        // unconditionally would reject test fixtures whose
        // `<notification>` payload omits the `urn:waddle:push:context:0`
        // child the chat publisher attaches in production.
        let web_push_provider_ready = self.web_push_provider_ready();
        let parsed = match dispatch::parse_publish_payload(&payload_xml) {
            Ok(parsed) => Some(parsed),
            Err(error) if web_push_provider_ready => {
                // Bad payload is permanent: mark the job failed in a
                // tiny dedicated tx and return zero attempts.
                self.mark_publish_job_failed_after_phase1(
                    job.job_id(),
                    job.owner_bare_jid(),
                    job.node(),
                    &format!("XEP-0357 payload parse failed: {error}"),
                    now_ms,
                )
                .await?;
                return Ok(Some(PushFanoutResult {
                    item_id: job.item_id().to_string(),
                    attempted_devices: 0,
                }));
            }
            // Provider-less tests historically use minimal XEP-0357
            // fixtures without Waddle context. Keep accepting them;
            // valid production payloads still parse above and supply
            // safe structured fields for the degraded-provider log.
            Err(_) => None,
        };
        let attempts = self
            .dispatch_devices(
                &sealed_devices,
                job.owner_bare_jid(),
                parsed.as_ref(),
                job.item_id(),
            )
            .await;

        // ---- Phase 3: tx2 — record attempts and finalize the job.
        let attempted_devices = attempts.len();
        self.finalize_publish_job(
            &job,
            &attempts,
            prior_delivered_devices,
            retention_limit,
            now_ms,
        )
        .await?;
        Ok(Some(PushFanoutResult {
            item_id: job.item_id().to_string(),
            attempted_devices,
        }))
    }

    /// Phase 1 of [`Self::process_publish_job_with_retention_limit`]:
    /// claim the job, validate node/owner/registration, ensure the
    /// XEP-0060 backing item exists, and load sealed devices + payload.
    /// Commits tx1 with the job still in
    /// [`PUBLISH_JOB_STATUS_IN_PROGRESS`] so the
    /// [`PUBLISH_JOB_CLAIM_TIMEOUT_MS`] claim window covers the
    /// out-of-tx Web Push round-trip.
    async fn process_publish_phase1(
        &self,
        job_id: &str,
        now_ms: i64,
    ) -> Result<Phase1Outcome, XmppError> {
        // `begin_immediate` so the SELECT-then-write sequence below
        // (read job + claim UPDATE + lock UPSERTs) acquires the SQLite
        // writer lock up front. With deferred begin, two concurrent
        // workers can both start as readers and then both try to
        // upgrade — SQLite returns `SQLITE_LOCKED` for the loser and
        // `busy_timeout` does not retry that case.
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        let Some(lock_target) = get_publish_job_tx(&mut tx, job_id).await? else {
            return Ok(Phase1Outcome::ShortCircuit(None));
        };
        lock_owner_tx(&mut tx, lock_target.owner_bare_jid(), now_ms).await?;
        lock_node_tx(&mut tx, lock_target.node(), now_ms).await?;
        let Some(job) = claim_publish_job_tx(&mut tx, job_id, now_ms).await? else {
            return Ok(Phase1Outcome::ShortCircuit(None));
        };
        let Some(push_node) = get_node_tx(&mut tx, job.node()).await? else {
            mark_publish_job_failed_tx(&mut tx, job.job_id(), "Push node not found", now_ms)
                .await?;
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(Phase1Outcome::ShortCircuit(Some(PushFanoutResult {
                item_id: job.item_id().to_string(),
                attempted_devices: 0,
            })));
        };
        if push_node.status != PushNodeStatus::Active {
            mark_publish_job_failed_tx(&mut tx, job.job_id(), "Push node not active", now_ms)
                .await?;
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(Phase1Outcome::ShortCircuit(Some(PushFanoutResult {
                item_id: job.item_id().to_string(),
                attempted_devices: 0,
            })));
        }
        if push_node.owner_bare_jid != *job.owner_bare_jid() {
            return Err(XmppError::forbidden(Some(
                "Push publish job owner does not match node owner".to_string(),
            )));
        }
        if let Some(push_service_jid) = job.push_service_jid() {
            if let Err(error) = ensure_active_registration_tx(
                &mut tx,
                job.owner_bare_jid(),
                push_service_jid,
                job.node(),
            )
            .await
            {
                if matches!(
                    error,
                    XmppError::Stanza {
                        condition: waddle_xmpp::StanzaErrorCondition::ItemNotFound,
                        ..
                    }
                ) {
                    mark_publish_job_failed_tx(
                        &mut tx,
                        job.job_id(),
                        "XEP-0357 registration not active",
                        now_ms,
                    )
                    .await?;
                    tx.commit()
                        .await
                        .map_err(|error| XmppError::internal(error.to_string()))?;
                    return Ok(Phase1Outcome::ShortCircuit(Some(PushFanoutResult {
                        item_id: job.item_id().to_string(),
                        attempted_devices: 0,
                    })));
                }
                return Err(error);
            }
        }
        self.ensure_xep0060_publish_item_backing(&job).await?;

        let sealed_devices =
            active_devices_with_subscription_for_node_tx(&mut tx, job.node()).await?;
        if sealed_devices.is_empty() {
            let retry_at_ms = retry_at_ms(now_ms);
            tx.execute(
                r#"
                UPDATE push_publish_jobs
                SET status = ?,
                    attempt_count = attempt_count + 1,
                    last_error = ?,
                    next_retry_at_ms = ?,
                    claimed_at_ms = NULL,
                    updated_at_ms = ?
                WHERE job_id = ? AND status = ?
                "#,
                crate::db_params![
                    PUBLISH_JOB_STATUS_QUEUED,
                    PUBLISH_JOB_ERROR_NO_ACTIVE_DEVICES,
                    retry_at_ms,
                    now_ms,
                    job.job_id().to_string(),
                    PUBLISH_JOB_STATUS_IN_PROGRESS,
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(Phase1Outcome::ShortCircuit(Some(PushFanoutResult {
                item_id: job.item_id().to_string(),
                attempted_devices: 0,
            })));
        }
        // #1123 per-device idempotency: a requeued job (one sibling
        // failed transiently) must not fan out again to devices whose
        // attempt for this same item already succeeded. Filter the
        // dispatch set against the terminal-success attempts recorded
        // by earlier passes.
        let already_delivered =
            delivered_device_ids_for_item_tx(&mut tx, job.node(), job.item_id()).await?;
        let device_count_before_filter = sealed_devices.len();
        let sealed_devices: Vec<_> = sealed_devices
            .into_iter()
            .filter(|device| !already_delivered.contains(&device.device_id))
            .collect();
        let prior_delivered_devices = device_count_before_filter - sealed_devices.len();
        if sealed_devices.is_empty() {
            // Every remaining active device already received this
            // item (the failing sibling was disabled or unregistered
            // between retries). The job is complete — finalize as
            // PUBLISHED instead of spinning in the no-active-devices
            // requeue loop. The `claim_token` predicate keeps the
            // transition at-most-once (see `finalize_publish_job`).
            tx.execute(
                r#"
                UPDATE push_publish_jobs
                SET status = ?,
                    attempt_count = attempt_count + 1,
                    last_error = NULL,
                    next_retry_at_ms = NULL,
                    claimed_at_ms = NULL,
                    claim_token = NULL,
                    updated_at_ms = ?,
                    published_at_ms = ?
                WHERE job_id = ? AND status = ? AND claim_token = ?
                "#,
                crate::db_params![
                    PUBLISH_JOB_STATUS_PUBLISHED,
                    now_ms,
                    now_ms,
                    job.job_id().to_string(),
                    PUBLISH_JOB_STATUS_IN_PROGRESS,
                    job.claim_token().to_string(),
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(Phase1Outcome::ShortCircuit(Some(PushFanoutResult {
                item_id: job.item_id().to_string(),
                attempted_devices: 0,
            })));
        }
        let payload_xml = get_publish_job_payload_xml_tx(&mut tx, job.job_id())
            .await?
            .ok_or_else(|| {
                XmppError::internal(format!(
                    "publish-job {} disappeared between claim and payload read",
                    job.job_id()
                ))
            })?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(Phase1Outcome::Continue(PublishWorkPhase1 {
            job,
            sealed_devices,
            payload_xml,
            prior_delivered_devices,
        }))
    }

    /// Phase 2 helper: drive each sealed device row through the typed
    /// Web Push dispatcher and collect typed [`DispatchedAttempt`]
    /// outcomes. Pure compute + network — never touches the DB.
    ///
    /// Devices are dispatched concurrently via `buffer_unordered` so a
    /// large fan-out does not serialize on the per-device HTTPS
    /// round-trip. The per-(host, urgency) leaky bucket and the global
    /// semaphore inside `RateLimitedWebPushSender` are the real rate-
    /// limiters; this cap exists to bound async-task overhead, not to
    /// rate-limit (cap > global semaphore size means extras simply
    /// block on the semaphore, which is fine).
    async fn dispatch_devices(
        &self,
        sealed_devices: &[dispatch::SealedActiveDevice],
        recipient: &BareJid,
        parsed: Option<&dispatch::ParsedPushPayload>,
        item_id: &str,
    ) -> Vec<DispatchedAttempt> {
        use futures::stream::{self, StreamExt};
        const DISPATCH_FAN_OUT: usize = 64;

        let web_provider = match (
            self.vapid_signer.as_ref(),
            self.web_push_sender.as_ref(),
            self.vapid_sub.as_ref(),
            parsed,
        ) {
            (Some(signer), Some(sender), Some(sub), Some(parsed)) => Some((
                Arc::clone(signer),
                Arc::clone(sender),
                sub.clone(),
                parsed.clone(),
                // Wrap the cipher in `Arc` so the per-device fan-out
                // clones an `Arc` refcount instead of duplicating the
                // (enc_key, mac_key) byte buffers up to 64× in heap.
                Arc::clone(&self.secrets),
            )),
            _ => None,
        };
        let item_id_arc: Arc<str> = Arc::from(item_id);
        let recipient = recipient.clone();
        // `Arc` so the per-device fan-out clones a refcount, not the
        // parsed payload, for the web-arm log context.
        let log_context = Arc::new(parsed.cloned());

        stream::iter(sealed_devices.iter().cloned())
            .map(move |device| {
                let item_id = Arc::clone(&item_id_arc);
                let web_provider = web_provider.clone();
                let recipient = recipient.clone();
                let log_context = Arc::clone(&log_context);
                async move {
                    match (&web_provider, device.platform) {
                        (Some((signer, sender, sub, parsed, secrets)), PushDevicePlatform::Web) => {
                            dispatch_web_device_owned(
                                &device,
                                &recipient,
                                parsed,
                                &item_id,
                                WebPushDispatchProvider {
                                    signer,
                                    sender,
                                    sub,
                                    secrets,
                                },
                            )
                            .await
                        }
                        // No Web Push provider wired up. Web devices
                        // get the typed `web-not-configured` marker
                        // (transient, so the job retries once the
                        // operator fixes the boot config); APNS/FCM
                        // devices retain the legacy `fake-sent` marker
                        // (their senders ship in #529 / #530).
                        (None, PushDevicePlatform::Web) => {
                            if let Some(parsed) = log_context.as_ref() {
                                tracing::warn!(
                                    recipient = %recipient,
                                    conversation = %parsed.conversation,
                                    notification_class = parsed.class.as_db_value(),
                                    provider = "web_push",
                                    push_stage = "provider_not_configured",
                                    provider_outcome = ATTEMPT_STATUS_WEB_NOT_CONFIGURED,
                                    "push provider transition"
                                );
                            }
                            DispatchedAttempt {
                                device_id: device.device_id.clone(),
                                platform: device.platform,
                                status: ATTEMPT_STATUS_WEB_NOT_CONFIGURED,
                                last_error: Some("Web Push provider not configured".to_string()),
                                retry_after: None,
                            }
                        }
                        (_, PushDevicePlatform::Apns | PushDevicePlatform::Fcm) => {
                            // APNS/FCM are stubbed until #529/#530 land
                            // their real senders. Keep the historical
                            // `fake-sent` marker so existing tests /
                            // dashboards keep working.
                            DispatchedAttempt {
                                device_id: device.device_id.clone(),
                                platform: device.platform,
                                status: dispatch::ATTEMPT_STATUS_FAKE_SENT_NON_WEB,
                                last_error: None,
                                retry_after: None,
                            }
                        }
                    }
                }
            })
            .buffer_unordered(DISPATCH_FAN_OUT)
            .collect::<Vec<_>>()
            .await
    }

    /// Phase 3: open a fresh tx, record one row per attempt, and either
    /// requeue the job (any transient outcome) or mark it published.
    /// Prunes the per-node attempts/jobs tail so retention stays bounded.
    ///
    /// At-most-once delivery is enforced via the `claim_token` UUID
    /// column: phase 1 mints a fresh token; phase 3's state-transition
    /// UPDATEs gate on `claim_token = ?` so a stale worker (whose
    /// claim was reset by `recover_stale_publish_job_claims`) sees 0
    /// rows changed and the *current* claim-holder owns the final
    /// state. The `push_delivery_attempts` INSERTs that come before
    /// the gating UPDATE are guarded by a token re-check at the head
    /// of the phase-3 tx — see the `verify_claim_token_or_abort_tx`
    /// call below.
    async fn finalize_publish_job(
        &self,
        job: &PushPublishJob,
        attempts: &[DispatchedAttempt],
        prior_delivered_devices: usize,
        retention_limit: i64,
        now_ms: i64,
    ) -> Result<(), XmppError> {
        // `begin_immediate` so phase 3 acquires the SQLite writer lock
        // up front; see the matching comment in `process_publish_phase1`.
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        lock_owner_tx(&mut tx, job.owner_bare_jid(), now_ms).await?;
        lock_node_tx(&mut tx, job.node(), now_ms).await?;
        // At-most-once interlock: if our claim was reset by a
        // stale-claim recovery between phase 1 and now, the row's
        // current `claim_token` no longer matches the one phase 1
        // captured. Abort BEFORE writing `push_delivery_attempts`
        // rows or disabling devices — those side effects belong to
        // the worker that holds the current claim. We still commit
        // the empty tx so the locks release cleanly.
        match read_publish_job_claim_token_tx(&mut tx, job.job_id()).await? {
            Some(current) if current == job.claim_token() => {}
            Some(_) | None => {
                tx.commit()
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                return Ok(());
            }
        }
        let mut any_device_disabled = false;
        for attempt in attempts {
            tx.execute(
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
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                crate::db_params![
                    uuid::Uuid::new_v4().to_string(),
                    job.node().to_string(),
                    attempt.device_id.clone(),
                    attempt.platform.to_string(),
                    job.item_id().to_string(),
                    attempt.status,
                    attempt.last_error.clone(),
                    now_ms,
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;

            // XEP-0357 §6 cleanup (forward direction):
            //
            //   "If a publish request is returned with an IQ-error,
            //    then the server SHOULD consider the particular JID
            //    and node combination to be disabled."
            //
            // The XEP frames this from the user-server's perspective,
            // but in our deployment the push service AND the
            // user-server are the same process. When the Web Push
            // relay tells us a subscription is permanently gone
            // (404/410), we mark the underlying `push_devices` row
            // disabled inside the same tx that records the gone
            // attempt — the next publish-job for this node will skip
            // the device and `count_active_devices_for_node_tx` will
            // see one fewer subscriber.
            //
            // We also disable on `web-invalid-keys` /
            // `web-invalid-endpoint`: the stored material is
            // structurally unusable, retrying with the same row is
            // pointless. `web-unseal-failed` is deliberately NOT
            // disabled — it usually signals a root-key drift across
            // boots and is fixable without the user re-registering.
            //
            // Reverse direction (notifying the chat client so it can
            // re-register a fresh subscription) lands in a follow-up
            // commit — see `dispatch_web_device_owned`.
            if attempt_status_warrants_device_disable(attempt.status) {
                mark_device_disabled_tx(
                    &mut tx,
                    job.node(),
                    &attempt.device_id,
                    attempt.status,
                    now_ms,
                )
                .await?;
                any_device_disabled = true;
            }
        }
        // XEP-0357 §6 forward cleanup (#775): §6 disables at
        // (JID, node) granularity, with §6.1 latitude to stay enabled
        // "until a sufficient number of errors have been received".
        // Our criterion: the node can no longer deliver at all — this
        // pass disabled a device AND no active device remains.
        // Transition the user-server registration in the SAME tx, so
        // the outbound target resolver (`get_for_user`, filtered on
        // status='enabled') stops producing candidates for this node
        // instead of enqueueing jobs that fail phase-1 forever.
        if any_device_disabled {
            if let Some(push_service_jid) = job.push_service_jid() {
                if count_active_devices_for_node_tx(&mut tx, job.node()).await? == 0 {
                    // Parse the stored service JID at this boundary —
                    // the job row predates typed storage; a malformed
                    // value is an internal invariant break, not a
                    // reason to skip §6 cleanup silently.
                    let push_service_jid: jid::BareJid =
                        push_service_jid.parse().map_err(|error| {
                            XmppError::internal(format!(
                                "stored push_service_jid is not a bare JID: {error}"
                            ))
                        })?;
                    let disabled = crate::push_registrations::disable_registration_tx(
                        &mut tx,
                        job.owner_bare_jid(),
                        &push_service_jid,
                        job.node(),
                        "XEP-0357 §6: all devices permanently unreachable (subscription gone/invalid)",
                    )
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                    if disabled > 0 {
                        tracing::info!(
                            owner = %job.owner_bare_jid(),
                            node = %job.node(),
                            "XEP-0357 §6 forward cleanup: last active device gone; registration disabled"
                        );
                    }
                }
            }
        }
        let any_transient = attempts
            .iter()
            .any(|attempt| attempt_status_is_transient(attempt.status));
        if any_transient {
            // Read the current `attempt_count` so we can enforce the
            // §6.1 "sufficient number of errors" cap. The row is
            // exclusively held by our claim (IN_PROGRESS); the read is
            // serializable.
            let attempt_count_so_far = read_publish_job_attempt_count_tx(&mut tx, job.job_id())
                .await?
                .unwrap_or(0);
            // Surface the first transient diagnostic so an operator can
            // see why this job is waiting to retry.
            let transient_error = attempts
                .iter()
                .find(|attempt| attempt_status_is_transient(attempt.status))
                .and_then(|attempt| attempt.last_error.clone())
                .unwrap_or_else(|| "Web Push transient failure".to_string());
            if attempt_count_so_far + 1 >= PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS {
                // XEP-0357 §6.1: "until a sufficient number of errors
                // have been received in a row." Past the ceiling, mark
                // the job permanently FAILED so it stops occupying the
                // queue. The operator's audit trail
                // (`push_delivery_attempts`) preserves every attempt.
                //
                // The `claim_token = ?` predicate is the at-most-once
                // interlock: if a stale-claim recovery + concurrent
                // re-claim happened between phase 1 and now, the row
                // holds a different token and this UPDATE matches 0
                // rows — our attempts insert is still durable, but
                // the job-state transition is the responsibility of
                // the worker that holds the *current* claim. Safe
                // either way.
                tx.execute(
                    r#"
                    UPDATE push_publish_jobs
                    SET status = ?,
                        attempt_count = attempt_count + 1,
                        last_error = ?,
                        next_retry_at_ms = NULL,
                        claimed_at_ms = NULL,
                        claim_token = NULL,
                        updated_at_ms = ?
                    WHERE job_id = ? AND status = ? AND claim_token = ?
                    "#,
                    crate::db_params![
                        PUBLISH_JOB_STATUS_FAILED,
                        truncate_last_error(&format!(
                            "transient retry cap exceeded ({PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS}); last: {transient_error}"
                        )),
                        now_ms,
                        job.job_id().to_string(),
                        PUBLISH_JOB_STATUS_IN_PROGRESS,
                        job.claim_token().to_string(),
                    ],
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            } else {
                // Honor the largest `Retry-After` carried by any
                // 429-style attempt. The default 60s floor still
                // applies — a relay that asks for 1s gets the 60s
                // backoff anyway — but a relay asking for 5min gets
                // 5min. Capped at 1h to bound runaway misbehavior.
                let mut retry_at = retry_at_ms(now_ms);
                if let Some(retry_after_ms) = attempts
                    .iter()
                    .filter_map(|attempt| attempt.retry_after)
                    .map(|d| d.as_millis().min(PUBLISH_JOB_MAX_RETRY_AFTER_MS as u128) as i64)
                    .max()
                {
                    let relay_deadline = now_ms.saturating_add(retry_after_ms);
                    retry_at = retry_at.max(relay_deadline);
                }
                tx.execute(
                    r#"
                    UPDATE push_publish_jobs
                    SET status = ?,
                        attempt_count = attempt_count + 1,
                        last_error = ?,
                        next_retry_at_ms = ?,
                        claimed_at_ms = NULL,
                        claim_token = NULL,
                        updated_at_ms = ?
                    WHERE job_id = ? AND status = ? AND claim_token = ?
                    "#,
                    crate::db_params![
                        PUBLISH_JOB_STATUS_QUEUED,
                        transient_error,
                        retry_at,
                        now_ms,
                        job.job_id().to_string(),
                        PUBLISH_JOB_STATUS_IN_PROGRESS,
                        job.claim_token().to_string(),
                    ],
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            }
        } else if let Some(uniform_status) = all_attempts_with_encoder_bug_signature(attempts)
            .filter(|_| prior_delivered_devices == 0)
        {
            // The `prior_delivered_devices == 0` guard (#1123, Codex
            // review): on a retry whose fan-out excluded devices that
            // already received the item, a uniform encoder-bug status
            // among the REMAINING devices is not "all devices" — the
            // payload demonstrably encoded and delivered for a
            // sibling, so the job completes as PUBLISHED (same
            // outcome a single mixed-result pass would produce).
            // Every device returned the same encoder-bug status —
            // either all `web-bad-request` (the relay rejected our
            // payload shape) or all `web-payload-too-large` (every
            // device exceeded the relay's per-message ceiling, which
            // is a padding/bucket-class misconfiguration). Both
            // recur identically on retry, so the job is a
            // deterministic failure, not a per-device problem.
            // Marking PUBLISHED would hide the regression behind a
            // "successful" job status; mark FAILED with the
            // diagnostic so monitoring on
            // `push_publish_jobs.status='failed'` surfaces it
            // without auditing the attempts table. Device rows
            // stay active — re-publish succeeds once the bug is
            // fixed.
            let uniform_error = attempts
                .iter()
                .find_map(|attempt| attempt.last_error.clone())
                .unwrap_or_else(|| format!("all devices returned {uniform_status}"));
            tx.execute(
                r#"
                UPDATE push_publish_jobs
                SET status = ?,
                    attempt_count = attempt_count + 1,
                    last_error = ?,
                    next_retry_at_ms = NULL,
                    claimed_at_ms = NULL,
                    claim_token = NULL,
                    updated_at_ms = ?
                WHERE job_id = ? AND status = ? AND claim_token = ?
                "#,
                crate::db_params![
                    PUBLISH_JOB_STATUS_FAILED,
                    truncate_last_error(&format!(
                        "all devices returned {uniform_status}; encoder/config bug suspected: {uniform_error}"
                    )),
                    now_ms,
                    job.job_id().to_string(),
                    PUBLISH_JOB_STATUS_IN_PROGRESS,
                    job.claim_token().to_string(),
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        } else {
            tx.execute(
                r#"
                UPDATE push_publish_jobs
                SET status = ?,
                    attempt_count = attempt_count + 1,
                    last_error = NULL,
                    next_retry_at_ms = NULL,
                    claimed_at_ms = NULL,
                    claim_token = NULL,
                    updated_at_ms = ?,
                    published_at_ms = ?
                WHERE job_id = ? AND status = ? AND claim_token = ?
                "#,
                crate::db_params![
                    PUBLISH_JOB_STATUS_PUBLISHED,
                    now_ms,
                    now_ms,
                    job.job_id().to_string(),
                    PUBLISH_JOB_STATUS_IN_PROGRESS,
                    job.claim_token().to_string(),
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        }
        prune_delivery_attempts_tx(&mut tx, job.node(), retention_limit).await?;
        prune_publish_jobs_tx(&mut tx, job.node(), MAX_PUBLISH_JOBS_PER_NODE).await?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(())
    }

    /// Mark a job that already passed phase 1 (so the
    /// `in-progress`/`claimed_at_ms` are set) as permanently failed in
    /// a tiny dedicated tx. Takes the same advisory locks as phase 3 so
    /// concurrent operations on the same owner/node serialize cleanly.
    async fn mark_publish_job_failed_after_phase1(
        &self,
        job_id: &str,
        owner_bare_jid: &BareJid,
        node: &str,
        error: &str,
        now_ms: i64,
    ) -> Result<(), XmppError> {
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        lock_owner_tx(&mut tx, owner_bare_jid, now_ms).await?;
        lock_node_tx(&mut tx, node, now_ms).await?;
        mark_publish_job_failed_tx(&mut tx, job_id, error, now_ms).await?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use crate::push_service::test_support::{notification_item, owner, store};
    use crate::push_service::{PushDevicePlatform, PushDeviceRegistration};

    #[tokio::test]
    async fn attempt_status_is_transient_matches_retry_intent() {
        // Lock the matrix down so a future enum reorganization can't
        // silently move `web-internal-error` (or any other permanent
        // status) back into the transient set. Permanent statuses
        // would spin in a 60s retry loop forever without the §6.1 cap;
        // even with the cap, classifying a deterministic bug as
        // transient burns 24 attempts before surfacing.
        for transient in [
            dispatch::ATTEMPT_STATUS_WEB_TRANSIENT,
            dispatch::ATTEMPT_STATUS_WEB_RATE_LIMITED,
            dispatch::ATTEMPT_STATUS_WEB_CLOCK_SKEW,
            ATTEMPT_STATUS_WEB_NOT_CONFIGURED,
        ] {
            assert!(
                attempt_status_is_transient(transient),
                "{transient} must be classified transient"
            );
        }
        for permanent in [
            dispatch::ATTEMPT_STATUS_WEB_DELIVERED,
            dispatch::ATTEMPT_STATUS_WEB_GONE,
            dispatch::ATTEMPT_STATUS_WEB_BAD_REQUEST,
            dispatch::ATTEMPT_STATUS_WEB_PAYLOAD_TOO_LARGE,
            dispatch::ATTEMPT_STATUS_WEB_INVALID_KEYS,
            dispatch::ATTEMPT_STATUS_WEB_INVALID_ENDPOINT,
            dispatch::ATTEMPT_STATUS_WEB_UNSEAL_FAILED,
            dispatch::ATTEMPT_STATUS_WEB_MISSING_MATERIAL,
            dispatch::ATTEMPT_STATUS_FAKE_SENT_NON_WEB,
            ATTEMPT_STATUS_WEB_INTERNAL_ERROR,
        ] {
            assert!(
                !attempt_status_is_transient(permanent),
                "{permanent} must NOT be classified transient — deterministic bugs and \
                 device-permanent failures should not requeue"
            );
        }
    }

    #[tokio::test]
    async fn warrants_disable_matches_xep0357_section_6_intent() {
        // §6: permanent unusable subscription → disable. Transient and
        // unseal-class errors → keep active. Lock the matrix down so a
        // future enum reorganization can't silently broaden or narrow
        // the disable trigger.
        assert!(attempt_status_warrants_device_disable(
            dispatch::ATTEMPT_STATUS_WEB_GONE
        ));
        assert!(attempt_status_warrants_device_disable(
            dispatch::ATTEMPT_STATUS_WEB_INVALID_KEYS
        ));
        assert!(attempt_status_warrants_device_disable(
            dispatch::ATTEMPT_STATUS_WEB_INVALID_ENDPOINT
        ));
        for never_disable in [
            dispatch::ATTEMPT_STATUS_WEB_DELIVERED,
            dispatch::ATTEMPT_STATUS_WEB_CLOCK_SKEW,
            dispatch::ATTEMPT_STATUS_WEB_RATE_LIMITED,
            dispatch::ATTEMPT_STATUS_WEB_TRANSIENT,
            dispatch::ATTEMPT_STATUS_WEB_BAD_REQUEST,
            dispatch::ATTEMPT_STATUS_WEB_PAYLOAD_TOO_LARGE,
            dispatch::ATTEMPT_STATUS_WEB_UNSEAL_FAILED,
            dispatch::ATTEMPT_STATUS_WEB_MISSING_MATERIAL,
            dispatch::ATTEMPT_STATUS_FAKE_SENT_NON_WEB,
            ATTEMPT_STATUS_WEB_NOT_CONFIGURED,
            ATTEMPT_STATUS_WEB_INTERNAL_ERROR,
        ] {
            assert!(
                !attempt_status_warrants_device_disable(never_disable),
                "{never_disable} must not trigger device disable"
            );
        }
    }

    #[tokio::test]
    async fn concurrent_publish_job_drains_claim_each_job_once() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("dev-1", node.node(), PushDevicePlatform::Apns, "test"),
            )
            .await
            .expect("device");

        let enqueue = store
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("claim-once"),
                &owner,
            )
            .await
            .expect("enqueue");
        assert!(enqueue.queued);

        let left_store = store.clone();
        let right_store = store.clone();
        let (left, right) = tokio::join!(
            async move { left_store.drain_queued_notification_publish_jobs(16).await },
            async move { right_store.drain_queued_notification_publish_jobs(16).await },
        );
        let left = left.expect("left drain");
        let right = right.expect("right drain");
        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");

        assert_eq!(left.len() + right.len(), 1);
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].item_id(), "claim-once");
        assert!(queued.is_empty());
    }

    #[tokio::test]
    async fn drain_continues_after_retryable_publish_job_failure() {
        let store = store().await;
        let owner = owner();
        let node = store.ensure_node(&owner, "web").await.expect("node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("dev-1", node.node(), PushDevicePlatform::Apns, "test"),
            )
            .await
            .expect("device");
        store
            .execute(
                r#"
                CREATE TRIGGER fail_poison_push_delivery_attempt
                BEFORE INSERT ON push_delivery_attempts
                WHEN NEW.item_id = 'poison'
                BEGIN
                    SELECT RAISE(ABORT, 'forced poison push delivery attempt failure');
                END
                "#,
                (),
            )
            .await
            .expect("failure trigger");
        for item_id in ["poison", "deliver-after-poison"] {
            store
                .enqueue_notification_publish_job_from_user_server(
                    node.node(),
                    &notification_item(item_id),
                    &owner,
                )
                .await
                .expect("enqueue");
        }

        let results = store
            .drain_queued_notification_publish_jobs(2)
            .await
            .expect("drain batch");
        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item_id(), "deliver-after-poison");
        assert_eq!(attempts.len(), 1);
        assert_eq!(attempts[0].item_id(), "deliver-after-poison");
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].item_id(), "poison");
    }

    #[tokio::test]
    async fn zero_device_retry_backoff_does_not_block_newer_jobs() {
        let store = store().await;
        let owner = owner();
        let zero_device_node = store.ensure_node(&owner, "web").await.expect("zero node");
        let live_node = store.ensure_node(&owner, "ios").await.expect("live node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new(
                    "ios-1",
                    live_node.node(),
                    PushDevicePlatform::Apns,
                    "test",
                ),
            )
            .await
            .expect("live device");
        store
            .enqueue_notification_publish_job_from_user_server(
                zero_device_node.node(),
                &notification_item("zero-device-oldest"),
                &owner,
            )
            .await
            .expect("enqueue zero-device");
        store
            .enqueue_notification_publish_job_from_user_server(
                live_node.node(),
                &notification_item("eligible-newer"),
                &owner,
            )
            .await
            .expect("enqueue eligible");

        let first = store
            .drain_queued_notification_publish_jobs(1)
            .await
            .expect("drain oldest");
        let second = store
            .drain_queued_notification_publish_jobs(1)
            .await
            .expect("drain next eligible");
        let live_attempts = store
            .delivery_attempts_for_node(live_node.node())
            .await
            .expect("live attempts");
        let queued = store.queued_publish_jobs().await.expect("queued jobs");
        let drained = first
            .iter()
            .chain(second.iter())
            .map(|result| (result.item_id().to_string(), result.attempted_devices()))
            .collect::<Vec<_>>();

        assert_eq!(first.len(), 1);
        assert_eq!(second.len(), 1);
        assert!(drained.contains(&("zero-device-oldest".to_string(), 0)));
        assert!(drained.contains(&("eligible-newer".to_string(), 1)));
        assert_eq!(live_attempts.len(), 1);
        assert_eq!(live_attempts[0].item_id(), "eligible-newer");
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].item_id(), "zero-device-oldest");
    }
}
