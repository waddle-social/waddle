//! Publish-job worker: the three-phase claim/dispatch/finalize pipeline
//! that fans a queued XEP-0357 notification out to registered devices,
//! plus attempt-status classification and XEP-0357 §6 device cleanup.

use std::sync::Arc;

use jid::BareJid;
use waddle_xmpp::push::types::VapidSub;
use waddle_xmpp::push::vapid::VapidSigner;
use waddle_xmpp::push::WebPushSender;
use waddle_xmpp::telemetry::attributes::{
    MetricAttribute, PushDisposition, PushFailureClass, PushRetryReason,
};
use waddle_xmpp::XmppError;

use super::apns_dispatch;
use super::devices::{
    active_devices_with_subscription_for_node_tx, count_active_devices_for_node_tx,
    mark_device_disabled_tx,
};
use super::dispatch;
use super::fcm_dispatch;
use super::nodes::get_node_tx;
use super::publish_jobs::{
    claim_publish_job_tx, delivered_device_ids_for_acceptance_tx, get_publish_job_payload_xml_tx,
    get_publish_job_tx, prune_delivery_attempts_tx, prune_publish_jobs_tx,
    read_publish_job_attempt_count_tx, read_publish_job_claim_token_tx,
    record_known_publish_job_failure_tx, retry_at_ms, MAX_DELIVERY_ATTEMPTS_PER_NODE,
    MAX_PUBLISH_JOBS_PER_NODE, PUBLISH_JOB_ERROR_NO_ACTIVE_DEVICES, PUBLISH_JOB_MAX_RETRY_AFTER_MS,
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
/// This path carries no retry metadata because the same input fails again.
const ATTEMPT_STATUS_WEB_INTERNAL_ERROR: &str = "web-internal-error";

/// Maximum number of characters persisted in
/// `push_delivery_attempts.last_error`. The column is `TEXT`, but we
/// keep diagnostics short so a runaway provider body cannot bloat the
/// table.
const PUSH_ATTEMPT_LAST_ERROR_MAX_CHARS: usize = 200;

/// Per-device outcome the publish-job worker carries from phase 2
/// (out-of-tx provider dispatch) back into phase 3 (the
/// `push_delivery_attempts` insert + job finalize). Keeping retry evidence typed
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
    /// Scheduling evidence from the typed provider result. `None` is terminal
    /// for this attempt; status strings are retained solely for persistence.
    retry: Option<dispatch::RetryMetadata>,
}

/// Output of phase 1 of the publish-job worker — the typed pieces
/// phase 2 needs to fan out Web Push deliveries without holding a DB
/// transaction.
struct PublishWorkPhase1 {
    job: PushPublishJob,
    sealed_devices: Vec<dispatch::SealedActiveDevice>,
    payload_xml: String,
    /// `push_nodes.app_id` of the job's node. For Apple devices this is
    /// the bundle id the registration was made for; APNs dispatch
    /// refuses to send when it differs from the configured topic.
    app_id: String,
}

/// Phase 1 can either continue into phase 2/3 with a [`PublishWorkPhase1`]
/// or short-circuit (claim contention, validation failure, no active
/// devices). The short-circuit variant carries the final
/// [`PushFanoutResult`] (or `None` when there was nothing to do).
enum Phase1Outcome {
    Continue(Box<PublishWorkPhase1>),
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
            TransientFailure::Connect => {
                "transient: connection setup failed before send".to_string()
            }
            TransientFailure::Network => "transient: network".to_string(),
            TransientFailure::ServerError { status } => {
                format!("transient: HTTP {status}")
            }
            TransientFailure::Timeout => "transient: timeout".to_string(),
        },
    }
}

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
    parsed: &dispatch::ParsedPushPayload,
    item_id: &str,
    provider: WebPushDispatchProvider<'_>,
) -> DispatchedAttempt {
    let target = match dispatch::WebPushTarget::try_from_sealed(device, provider.secrets) {
        Ok(target) => target,
        Err(reason) => {
            let status = dispatch::skip_reason_to_attempt_status(reason);
            tracing::warn!(
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
                retry: None,
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
                    notification_class = parsed.class.as_db_value(),
                    provider = "web_push",
                    push_stage = stage.value(),
                    provider_outcome = status,
                    "push provider transition"
                ),
                None => tracing::warn!(
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
                retry: dispatch::web_push_retry_metadata(&outcome),
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
                retry: None,
            }
        }
    }
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
/// - `apns-gone` is the APNs equivalent of `web-gone`: 410
///   (`Unregistered` / `ExpiredToken`), `BadDeviceToken` or
///   `DeviceTokenNotForTopic`. `apns-invalid-token` is a stored token
///   that is not hex, the APNs twin of `web-invalid-keys`.
/// - Provider-wide APNs failures (`apns-provider-auth`,
///   `apns-topic-mismatch`, `apns-not-configured`) are deliberately
///   NOT listed: they are server configuration errors that would
///   otherwise disable every Apple device at once.
/// - `fcm-gone` (`UNREGISTERED`) and `fcm-invalid-token` (FCM blamed
///   `message.token`, or the stored token is not a registration token)
///   are the FCM token-death statuses; `fcm-invalid-keys` is the FCM
///   twin of `web-invalid-keys`. Provider-wide FCM failures
///   (`fcm-provider-auth`, `fcm-sender-id-mismatch`,
///   `fcm-app-id-mismatch`, `fcm-not-configured`), including a bare
///   `404` without an FCM error code (an unknown project), are NOT
///   listed.
/// - Transient statuses are handled by the retry path; we keep the
///   device row active so the next attempt has somewhere to land.
fn attempt_status_warrants_device_disable(status: &str) -> bool {
    matches!(
        status,
        dispatch::ATTEMPT_STATUS_WEB_GONE
            | dispatch::ATTEMPT_STATUS_WEB_INVALID_KEYS
            | dispatch::ATTEMPT_STATUS_WEB_INVALID_ENDPOINT
            | apns_dispatch::ATTEMPT_STATUS_APNS_GONE
            | apns_dispatch::ATTEMPT_STATUS_APNS_INVALID_TOKEN
            | fcm_dispatch::ATTEMPT_STATUS_FCM_GONE
            | fcm_dispatch::ATTEMPT_STATUS_FCM_INVALID_TOKEN
            | fcm_dispatch::ATTEMPT_STATUS_FCM_INVALID_KEYS
    )
}

/// If every attempt in the fan-out carries the same encoder/config-bug
/// status (e.g. all `web-bad-request`, all `web-payload-too-large`, or
/// all of one APNs provider-wide failure), return that status so the
/// worker can finalize the job as FAILED instead of silently marking it
/// PUBLISHED. Returns `None` otherwise.
///
/// These statuses are deterministic on the fan-out: every device hits
/// the same encoder/padding bug or server misconfiguration (APNs key,
/// bundle id, missing `WADDLE_APNS_*`; FCM credentials, project or app
/// id, missing `WADDLE_FCM_*`), so the job will recur
/// identically on retry. Surfacing FAILED on `push_publish_jobs.status`
/// lets monitors alert without auditing the attempts table.
fn all_attempts_with_encoder_bug_signature(attempts: &[DispatchedAttempt]) -> Option<&'static str> {
    if attempts.is_empty() {
        return None;
    }
    let first = attempts[0].status;
    let uniform = matches!(
        first,
        dispatch::ATTEMPT_STATUS_WEB_BAD_REQUEST
            | dispatch::ATTEMPT_STATUS_WEB_PAYLOAD_TOO_LARGE
            | apns_dispatch::ATTEMPT_STATUS_APNS_REJECTED
            | apns_dispatch::ATTEMPT_STATUS_APNS_PAYLOAD_TOO_LARGE
            | apns_dispatch::ATTEMPT_STATUS_APNS_PROVIDER_AUTH
            | apns_dispatch::ATTEMPT_STATUS_APNS_TOPIC_MISMATCH
            | apns_dispatch::ATTEMPT_STATUS_APNS_NOT_CONFIGURED
            | fcm_dispatch::ATTEMPT_STATUS_FCM_REJECTED
            | fcm_dispatch::ATTEMPT_STATUS_FCM_PAYLOAD_TOO_LARGE
            | fcm_dispatch::ATTEMPT_STATUS_FCM_PROVIDER_AUTH
            | fcm_dispatch::ATTEMPT_STATUS_FCM_SENDER_ID_MISMATCH
            | fcm_dispatch::ATTEMPT_STATUS_FCM_APP_ID_MISMATCH
            | fcm_dispatch::ATTEMPT_STATUS_FCM_NOT_CONFIGURED
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
                    // No claim token escapes this processing call. Preserve an
                    // in-progress lease and its possible-send uncertainty.
                    self.record_publish_job_failure_by_id(&job_id, &error.to_string())
                        .await?;
                }
            }
        }
        // Known pre-send failures may have stopped scheduling while a canonical
        // parent was locked. Repair their custody after fresh work has run;
        // failure here must not discard successful results from this batch.
        if let Err(error) = self.reconcile_failed_publish_job_custody(limit).await {
            tracing::warn!(%error, "known push failure custody reconciliation deferred");
        }
        Ok(results)
    }

    pub(super) async fn process_publish_job_with_retention_limit(
        &self,
        job_id: &str,
        retention_limit: i64,
    ) -> Result<Option<PushFanoutResult>, XmppError> {
        self.recover_stale_publish_job_claim_by_id(job_id).await?;
        self.complete_versioned_publish_backing(job_id).await?;
        let now_ms = crate::time::now_ms();

        // ---- Phase 1: tx1 — claim + validate + load sealed devices +
        // read payload_xml. We commit with the job still in
        // `in-progress` so its `PUBLISH_JOB_CLAIM_TIMEOUT_MS` claim
        // window covers phases 2+3 without holding a DB transaction
        // across the network round-trip.
        let phase1 = match self.process_publish_phase1(job_id, now_ms).await? {
            Phase1Outcome::Continue(state) => *state,
            Phase1Outcome::ShortCircuit(result) => return Ok(result),
        };
        let PublishWorkPhase1 {
            job,
            sealed_devices,
            payload_xml,
            app_id,
        } = phase1;

        // ---- Phase 2: outside any tx — encrypt, sign, and send.
        // The XEP-0357 payload only needs to be parsed when a real
        // provider (Web Push, APNs or FCM) is wired up. Without one,
        // every device records a not-configured marker and we never
        // look at the conversation / class / message-count fields.
        // Parsing unconditionally would reject test fixtures whose
        // `<notification>` payload omits the `urn:waddle:push:context:0`
        // child the chat publisher attaches in production.
        let any_provider_ready = self.any_provider_ready();
        let parsed = match dispatch::parse_publish_payload(&payload_xml) {
            Ok(parsed) => Some(parsed),
            Err(error) if any_provider_ready => {
                // This attempt cannot send. Only a job without earlier
                // uncertainty may take a terminal failure disposition.
                self.mark_publish_job_failed_after_phase1(
                    &job,
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
                apns_dispatch::ApnsJobContext {
                    item_id: job.item_id(),
                    node: job.node(),
                    app_id: &app_id,
                },
            )
            .await;

        // ---- Phase 3: tx2 — record attempts and finalize the job.
        let attempted_devices = attempts.len();
        self.finalize_publish_job(&job, &attempts, retention_limit, crate::time::now_ms())
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
        super::publish_jobs::lock_notification_ancestry_tx(&mut tx, &lock_target).await?;
        lock_owner_tx(&mut tx, lock_target.owner_bare_jid(), now_ms).await?;
        lock_node_tx(&mut tx, lock_target.node(), now_ms).await?;
        let Some(job) = claim_publish_job_tx(&mut tx, job_id, now_ms).await? else {
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(Phase1Outcome::ShortCircuit(None));
        };
        let Some(push_node) = get_node_tx(&mut tx, job.node()).await? else {
            record_known_publish_job_failure_tx(&mut tx, &job, "Push node not found", now_ms)
                .await?;
            super::publish_jobs::settle_terminal_notification_ancestry_tx(&mut tx, &job).await?;
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            return Ok(Phase1Outcome::ShortCircuit(Some(PushFanoutResult {
                item_id: job.item_id().to_string(),
                attempted_devices: 0,
            })));
        };
        if push_node.status != PushNodeStatus::Active {
            record_known_publish_job_failure_tx(&mut tx, &job, "Push node not active", now_ms)
                .await?;
            super::publish_jobs::settle_terminal_notification_ancestry_tx(&mut tx, &job).await?;
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
                push_service_jid.as_str(),
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
                    record_known_publish_job_failure_tx(
                        &mut tx,
                        &job,
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

        let sealed_devices =
            active_devices_with_subscription_for_node_tx(&mut tx, job.node()).await?;
        // Delivery proof belongs to this durable acceptance, even when every
        // device has since become inactive. Check it before the empty-node
        // retry path so an already delivered notification can complete.
        let already_delivered =
            delivered_device_ids_for_acceptance_tx(&mut tx, job.job_id()).await?;
        if sealed_devices.is_empty() && already_delivered.is_empty() {
            let failed_attempt_count = read_publish_job_attempt_count_tx(&mut tx, job.job_id())
                .await?
                .unwrap_or(0)
                + 1;
            let exhausted = failed_attempt_count >= PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS;
            let status = if exhausted {
                PUBLISH_JOB_STATUS_FAILED
            } else {
                PUBLISH_JOB_STATUS_QUEUED
            };
            let retry_at_ms = (!exhausted).then(|| retry_at_ms(now_ms, failed_attempt_count));
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
                WHERE job_id = ? AND status = ?
                "#,
                crate::db_params![
                    status,
                    PUBLISH_JOB_ERROR_NO_ACTIVE_DEVICES,
                    retry_at_ms,
                    now_ms,
                    job.job_id().to_string(),
                    PUBLISH_JOB_STATUS_IN_PROGRESS,
                ],
            )
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
            super::publish_jobs::settle_terminal_notification_ancestry_tx(&mut tx, &job).await?;
            tx.commit()
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            waddle_xmpp::telemetry::reliability::increment_push_disposition(
                PushRetryReason::PreSend,
                PushFailureClass::PreSend,
                if !exhausted {
                    PushDisposition::Retry
                } else if job.uncertain_send {
                    PushDisposition::Parked
                } else {
                    PushDisposition::DeadLetter
                },
            );
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
        let sealed_devices: Vec<_> = sealed_devices
            .into_iter()
            .filter(|device| !already_delivered.contains(&device.device_id))
            .collect();
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
            super::publish_jobs::settle_terminal_notification_ancestry_tx(&mut tx, &job).await?;
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
        Ok(Phase1Outcome::Continue(Box::new(PublishWorkPhase1 {
            job,
            sealed_devices,
            payload_xml,
            app_id: push_node.app_id,
        })))
    }

    async fn current_apns_badge_count(&self, recipient: &BareJid) -> Option<u64> {
        let Some(inbox_storage) = self.inbox_storage.as_ref() else {
            tracing::warn!("APNs app badge omitted because inbox storage is unavailable");
            return None;
        };

        match inbox_storage.total_unread(recipient).await {
            Ok(total) => Some(total),
            Err(_) => {
                tracing::warn!(
                    "APNs app badge omitted because account unread total could not be read"
                );
                None
            }
        }
    }

    /// Phase 2 helper: read the current account unread total once for
    /// APNs badge delivery, then drive each sealed device row through the
    /// typed provider dispatchers and collect [`DispatchedAttempt`]s. The
    /// shared inbox read completes before the network fan-out; no push-store
    /// transaction is held across provider requests.
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
        job: apns_dispatch::ApnsJobContext<'_>,
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
        // APNs needs the parsed payload too; a parse failure with any
        // provider wired already failed the job in phase 2.
        let has_apns_device = sealed_devices
            .iter()
            .any(|device| device.platform == PushDevicePlatform::Apns);
        let badge_count = if self.apns.is_some() && parsed.is_some() && has_apns_device {
            self.current_apns_badge_count(recipient).await
        } else {
            None
        };
        let apns_provider = match (self.apns.as_ref(), parsed) {
            (Some(provider), Some(parsed)) => Some((provider.clone(), parsed.clone(), badge_count)),
            _ => None,
        };
        // FCM: one OAuth access token for the whole fan-out, fetched
        // before any device is contacted (like the badge read above).
        // Not fetched when the node belongs to another app: every device
        // then records `fcm-app-id-mismatch` without unsealing anything.
        let has_fcm_device = sealed_devices
            .iter()
            .any(|device| device.platform == PushDevicePlatform::Fcm);
        let fcm_provider = match (self.fcm.as_ref(), parsed) {
            (Some(provider), Some(parsed)) => {
                let access_token = if has_fcm_device && provider.serves_app_id(job.app_id) {
                    Some(provider.tokens.current().await)
                } else {
                    None
                };
                Some(Arc::new((provider.clone(), parsed.clone(), access_token)))
            }
            _ => None,
        };
        let item_id_arc: Arc<str> = Arc::from(job.item_id);
        let node_arc: Arc<str> = Arc::from(job.node);
        let app_id_arc: Arc<str> = Arc::from(job.app_id);
        let secrets = Arc::clone(&self.secrets);
        // `Arc` so the per-device fan-out clones a refcount, not the
        // parsed payload, for the web-arm log context.
        let log_context = Arc::new(parsed.cloned());

        stream::iter(sealed_devices.iter().cloned())
            .map(move |device| {
                let item_id = Arc::clone(&item_id_arc);
                let node = Arc::clone(&node_arc);
                let app_id = Arc::clone(&app_id_arc);
                let secrets = Arc::clone(&secrets);
                let web_provider = web_provider.clone();
                let apns_provider = apns_provider.clone();
                let fcm_provider = fcm_provider.clone();
                let log_context = Arc::clone(&log_context);
                async move {
                    match (&web_provider, device.platform) {
                        (Some((signer, sender, sub, parsed, secrets)), PushDevicePlatform::Web) => {
                            dispatch_web_device_owned(
                                &device,
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
                        // operator fixes the boot config).
                        (None, PushDevicePlatform::Web) => {
                            if let Some(parsed) = log_context.as_ref() {
                                tracing::warn!(
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
                                retry: Some(dispatch::RetryMetadata::PRE_SEND),
                            }
                        }
                        (_, PushDevicePlatform::Apns) => match &apns_provider {
                            Some((provider, parsed, badge_count)) => {
                                let attempt = apns_dispatch::dispatch_apns_device(
                                    &device,
                                    parsed,
                                    *badge_count,
                                    apns_dispatch::ApnsJobContext {
                                        item_id: &item_id,
                                        node: &node,
                                        app_id: &app_id,
                                    },
                                    provider,
                                    &secrets,
                                )
                                .await;
                                DispatchedAttempt {
                                    device_id: device.device_id.clone(),
                                    platform: device.platform,
                                    status: attempt.status,
                                    last_error: attempt
                                        .last_error
                                        .map(|error| truncate_last_error(&error)),
                                    retry_after: attempt.retry_after,
                                    retry: attempt.retry,
                                }
                            }
                            // No `WADDLE_APNS_*` configuration. Recorded
                            // as a permanent, non-disabling status: the
                            // device is fine and retrying cannot help
                            // until the operator configures APNs.
                            None => {
                                if let Some(parsed) = log_context.as_ref() {
                                    tracing::warn!(
                                        notification_class = parsed.class.as_db_value(),
                                        provider = "apns",
                                        push_stage = "provider_not_configured",
                                        provider_outcome =
                                            apns_dispatch::ATTEMPT_STATUS_APNS_NOT_CONFIGURED,
                                        "push provider transition"
                                    );
                                }
                                DispatchedAttempt {
                                    device_id: device.device_id.clone(),
                                    platform: device.platform,
                                    status: apns_dispatch::ATTEMPT_STATUS_APNS_NOT_CONFIGURED,
                                    last_error: Some("APNs provider not configured".to_string()),
                                    retry_after: None,
                                    retry: None,
                                }
                            }
                        },
                        (_, PushDevicePlatform::Fcm) => match fcm_provider.as_deref() {
                            Some((provider, parsed, access_token)) => {
                                let attempt = fcm_dispatch::dispatch_fcm_device(
                                    &device,
                                    parsed,
                                    fcm_dispatch::FcmJobContext {
                                        item_id: &item_id,
                                        app_id: &app_id,
                                    },
                                    provider,
                                    &secrets,
                                    access_token.as_ref(),
                                )
                                .await;
                                DispatchedAttempt {
                                    device_id: device.device_id.clone(),
                                    platform: device.platform,
                                    status: attempt.status,
                                    last_error: attempt
                                        .last_error
                                        .map(|error| truncate_last_error(&error)),
                                    retry_after: attempt.retry_after,
                                    retry: attempt.retry,
                                }
                            }
                            // No `WADDLE_FCM_*` configuration. Permanent
                            // and non-disabling, like `apns-not-configured`.
                            None => {
                                if let Some(parsed) = log_context.as_ref() {
                                    tracing::warn!(
                                        notification_class = parsed.class.as_db_value(),
                                        provider = "fcm",
                                        push_stage = "provider_not_configured",
                                        provider_outcome =
                                            fcm_dispatch::ATTEMPT_STATUS_FCM_NOT_CONFIGURED,
                                        "push provider transition"
                                    );
                                }
                                DispatchedAttempt {
                                    device_id: device.device_id.clone(),
                                    platform: device.platform,
                                    status: fcm_dispatch::ATTEMPT_STATUS_FCM_NOT_CONFIGURED,
                                    last_error: Some("FCM provider not configured".to_string()),
                                    retry_after: None,
                                    retry: None,
                                }
                            }
                        },
                    }
                }
            })
            .buffer_unordered(DISPATCH_FAN_OUT)
            .collect::<Vec<_>>()
            .await
    }

    /// Phase 3: open a fresh tx, record one row per attempt, and either
    /// requeue within budget, park unresolved work, or persist terminal outcome.
    /// Prunes the per-node attempts/jobs tail so retention stays bounded.
    ///
    /// Completion writes are fenced by the `claim_token` UUID
    /// column. Unknown provider sends are duplicate-possible until their bounded
    /// retry budget expires; then they remain parked with custody retained.
    /// Phase 1 mints a fresh token; phase 3's state-transition
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
        super::publish_jobs::lock_notification_ancestry_tx(&mut tx, job).await?;
        lock_owner_tx(&mut tx, job.owner_bare_jid(), now_ms).await?;
        lock_node_tx(&mut tx, job.node(), now_ms).await?;
        // Completion interlock: if our claim was reset by a
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
        let prior_uncertainty = get_publish_job_tx(&mut tx, job.job_id())
            .await?
            .ok_or_else(|| XmppError::internal("claimed publish job missing"))?
            .uncertain_send;
        let mut any_device_disabled = false;
        for attempt in attempts {
            tx.execute(
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
                ) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)
                "#,
                crate::db_params![
                    job.job_id(),
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
                    let disabled = crate::push_registrations::disable_registration_tx(
                        &mut tx,
                        job.owner_bare_jid(),
                        push_service_jid,
                        job.node(),
                        "XEP-0357 §6: all devices permanently unreachable (subscription gone/invalid)",
                    )
                    .await
                    .map_err(|error| XmppError::internal(error.to_string()))?;
                    if disabled > 0 {
                        tracing::info!(

                            "XEP-0357 §6 forward cleanup: last active device gone; registration disabled"
                        );
                    }
                }
            }
        }
        // Only this scheduler acceptance's confirmed deliveries resolve its
        // logical notification. Include successes from this fenced pass as
        // well as earlier retries, even if those devices are no longer active.
        let has_confirmed_delivery = !delivered_device_ids_for_acceptance_tx(&mut tx, job.job_id())
            .await?
            .is_empty();
        // Prefer the least definitive evidence from this fan-out. Earlier
        // uncertainty remains separate from the current attempt's classification.
        let retry_attempt = attempts
            .iter()
            .filter_map(|attempt| attempt.retry.map(|retry| (attempt, retry)))
            .max_by_key(|(_, retry)| match retry.failure_class {
                PushFailureClass::PreSend => 0,
                PushFailureClass::ProvenRefusal => 1,
                PushFailureClass::UnknownAfterSend => 2,
            });
        let mut retry_disposition = None;
        if let Some((retry_attempt, retry)) = retry_attempt {
            // Read the current `attempt_count` so we can enforce the
            // §6.1 "sufficient number of errors" cap. The row is
            // exclusively held by our claim (IN_PROGRESS); the read is
            // serializable.
            let attempt_count_so_far = read_publish_job_attempt_count_tx(&mut tx, job.job_id())
                .await?
                .unwrap_or(0);
            let transient_error = retry_attempt
                .last_error
                .clone()
                .unwrap_or_else(|| "Push transient failure".to_string());
            let failure_class = retry.failure_class;
            let reason = retry.reason;
            // A later pre-send failure or proven refusal cannot erase an
            // earlier possible provider acceptance.
            let unknown_send =
                prior_uncertainty || failure_class == PushFailureClass::UnknownAfterSend;
            let exhausted = attempt_count_so_far + 1 >= PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS;
            let disposition = if !exhausted {
                PushDisposition::Retry
            } else if unknown_send {
                PushDisposition::Parked
            } else {
                PushDisposition::DeadLetter
            };
            retry_disposition = Some((
                reason,
                failure_class,
                disposition,
                attempt_count_so_far + 1,
                unknown_send,
                transient_error.clone(),
            ));
            if exhausted {
                // XEP-0357 §6.1: "until a sufficient number of errors
                // have been received in a row." The ceiling always removes
                // the job from dispatch. Unknown sends retain uncertain_send:
                // their failed status is parked custody, not terminal proof.
                // The operator's audit trail
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
                        uncertain_send = ?,
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
                        i64::from(unknown_send),
                        now_ms,
                        job.job_id().to_string(),
                        PUBLISH_JOB_STATUS_IN_PROGRESS,
                        job.claim_token().to_string(),
                    ],
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            } else {
                // Honor the largest Retry-After above the exponential
                // scheduler backoff. Both remain capped at one hour.
                let mut retry_at = retry_at_ms(now_ms, attempt_count_so_far + 1);
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
                        uncertain_send = ?,
                        claimed_at_ms = NULL,
                        claim_token = NULL,
                        updated_at_ms = ?
                    WHERE job_id = ? AND status = ? AND claim_token = ?
                    "#,
                    crate::db_params![
                        PUBLISH_JOB_STATUS_QUEUED,
                        transient_error,
                        retry_at,
                        i64::from(unknown_send),
                        now_ms,
                        job.job_id().to_string(),
                        PUBLISH_JOB_STATUS_IN_PROGRESS,
                        job.claim_token().to_string(),
                    ],
                )
                .await
                .map_err(|error| XmppError::internal(error.to_string()))?;
            }
        } else if prior_uncertainty
            && !has_confirmed_delivery
            && (attempts.is_empty()
                || attempts.iter().any(|attempt| {
                    !matches!(
                        attempt.status,
                        dispatch::ATTEMPT_STATUS_WEB_DELIVERED
                            | apns_dispatch::ATTEMPT_STATUS_APNS_DELIVERED
                            | fcm_dispatch::ATTEMPT_STATUS_FCM_DELIVERED
                    )
                }))
        {
            let error = attempts
                .iter()
                .find_map(|attempt| attempt.last_error.as_deref())
                .unwrap_or("No definitive provider success after an earlier unknown send");
            if let Some(failure) =
                record_known_publish_job_failure_tx(&mut tx, job, error, now_ms).await?
            {
                // No current retry evidence exists. The earlier unresolved
                // acceptance is why this job still retries or parks; do not
                // reclassify the current nonretry provider outcome.
                retry_disposition = Some((
                    PushRetryReason::UnknownAfterSend,
                    PushFailureClass::UnknownAfterSend,
                    failure.disposition,
                    failure.attempt_count,
                    failure.uncertain_send,
                    error.to_string(),
                ));
            }
        } else if let Some(uniform_status) =
            all_attempts_with_encoder_bug_signature(attempts).filter(|_| !has_confirmed_delivery)
        {
            // A confirmed delivery for this acceptance (#1123) means a
            // uniform encoder-bug status among remaining devices is not an
            // all-device failure. The job then completes as PUBLISHED,
            // matching a single pass with mixed success/permanent outcomes.
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
        super::publish_jobs::settle_terminal_notification_ancestry_tx(&mut tx, job).await?;
        prune_delivery_attempts_tx(&mut tx, job.node(), retention_limit).await?;
        prune_publish_jobs_tx(&mut tx, job.node(), MAX_PUBLISH_JOBS_PER_NODE).await?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        if let Some((reason, failure_class, disposition, attempt_count, uncertain_send, error)) =
            retry_disposition
        {
            waddle_xmpp::telemetry::reliability::increment_push_disposition(
                reason,
                failure_class,
                disposition,
            );
            tracing::warn!(reason = reason.as_str(), failure_class = failure_class.as_str(), disposition = disposition.as_str(), attempt_count, uncertain_send, error = %error, "push provider retry disposition");
        }
        Ok(())
    }

    /// Record a pre-send failure using the claim captured by phase 1. Earlier
    /// uncertainty remains retained; a successor claim cannot be mutated.
    async fn mark_publish_job_failed_after_phase1(
        &self,
        job: &PushPublishJob,
        error: &str,
        now_ms: i64,
    ) -> Result<(), XmppError> {
        let mut tx = self
            .db
            .begin_immediate()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        super::publish_jobs::lock_notification_ancestry_tx(&mut tx, job).await?;
        lock_owner_tx(&mut tx, job.owner_bare_jid(), now_ms).await?;
        lock_node_tx(&mut tx, job.node(), now_ms).await?;
        record_known_publish_job_failure_tx(&mut tx, job, error, now_ms).await?;
        tx.commit()
            .await
            .map_err(|error| XmppError::internal(error.to_string()))?;
        Ok(())
    }
}

#[cfg(test)]
#[path = "worker_uncertainty_tests.rs"]
mod uncertainty_tests;

#[cfg(test)]
mod tests {
    use super::*;

    use crate::push_service::test_support::{
        fcm_registration, notification_item, owner, store, store_with_fake_fcm, FCM_TEST_APP_ID,
    };
    use crate::push_service::{PushDevicePlatform, PushDeviceRegistration};
    use waddle_xmpp::inbox::storage::InboxStorage;

    async fn missing_backing_cap_releases_all_canonical_parents(
        fixture: crate::ingress::test_support::IngressFixture,
    ) {
        use crate::ingress_substrate::{gc_expired_aliases, AliasGcBudget};
        use crate::ingress_uow::{
            settle_recorded, CanonicalMessageRepository, EffectDescendantRepository,
            EffectIntentRepository,
        };
        use waddle_xmpp::ingress::{
            IngressEffectIntent, MessageKey, NotificationActivityMutation,
            NotificationCandidateOutcome, SemanticDigest,
        };
        use waddle_xmpp::pubsub::PubSubStorage;

        let backing = Arc::new(
            crate::pubsub::DatabasePubSubStorage::open(Some("sqlite::memory:"))
                .await
                .expect("independent backing"),
        );
        let service: BareJid = "push.example.com".parse().expect("service");
        crate::push_registrations::DatabasePushRegistrationStore::new(fixture.db.clone())
            .await
            .expect("registration store");
        let push = DatabasePushServiceStore::new_with_secret_key_and_pubsub(
            fixture.db.clone(),
            &rand::random::<[u8; 32]>(),
            service.clone(),
            backing.clone(),
        )
        .await
        .expect("store");
        let owner = owner();
        let node = push
            .ensure_node(&owner, "missing-backing")
            .await
            .expect("node");
        push.upsert_device(
            &owner,
            PushDeviceRegistration::new("device", node.node(), PushDevicePlatform::Fcm, "test"),
        )
        .await
        .expect("device");
        push.register_first_party_node_for_owner(&owner, service.as_str(), node.node(), None)
            .await
            .expect("registration");
        let delivery = uuid::Uuid::new_v4();
        let accepted = push
            .enqueue_canonical_notification_publish_job(
                node.node(),
                &notification_item("missing-backing"),
                &owner,
                &service,
                None,
                delivery,
            )
            .await
            .expect("canonical acceptance");
        let intent = IngressEffectIntent::NotificationActivityPreview {
            owner: owner.clone(),
            mutation: NotificationActivityMutation::NotificationCandidate {
                conversation: "bob@example.com".parse().expect("conversation"),
                archive_stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
                    "missing-backing",
                    owner.clone().into(),
                ),
                outcome: NotificationCandidateOutcome::Inserted,
            },
        };
        for _ in 0..2 {
            let key = MessageKey::new();
            let mut tx = fixture.uow.begin().await.expect("canonical tx");
            CanonicalMessageRepository::record_message(
                &mut tx,
                key,
                &SemanticDigest::from_storage(1, [1; 32]).expect("digest"),
                None,
            )
            .await
            .expect("parent");
            EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
                .await
                .expect("intent");
            EffectDescendantRepository::attach(&mut tx, key, &intent.semantic_key(), delivery)
                .await
                .expect("provider custody");
            settle_recorded(&mut tx, key, std::slice::from_ref(&intent))
                .await
                .expect("candidate insertion proof");
            CanonicalMessageRepository::terminalize(&mut tx, key, chrono::Utc::now())
                .await
                .expect("canonical obligations complete");
            tx.commit().await.expect("commit parents");
        }
        backing
            .delete_node(&service, node.node())
            .await
            .expect("remove backing");
        fixture
            .execute(
                "UPDATE push_publish_jobs SET attempt_count = ? WHERE job_id = ?",
                crate::db_params![
                    PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS - 2,
                    accepted.job_id().to_string()
                ],
            )
            .await;
        assert_eq!(
            fixture
                .count("ingress_effect_descendants WHERE settled_at IS NULL")
                .await,
            2
        );
        assert_eq!(
            fixture
                .count("ingress_messages WHERE retention_eligible_at IS NOT NULL")
                .await,
            0
        );
        assert!(push
            .drain_queued_notification_publish_jobs(16)
            .await
            .expect("penultimate retry")
            .is_empty());
        assert_eq!(push.queued_publish_jobs().await.expect("queue").len(), 1);
        assert_eq!(
            fixture
                .count("ingress_effect_descendants WHERE settled_at IS NULL")
                .await,
            2
        );
        fixture
            .execute(
                "UPDATE push_publish_jobs SET next_retry_at_ms = NULL WHERE job_id = ?",
                crate::db_params![accepted.job_id().to_string()],
            )
            .await;
        assert!(push
            .drain_queued_notification_publish_jobs(16)
            .await
            .expect("cap")
            .is_empty());
        assert_eq!(
            push.load_publish_job(&accepted.job_id().to_string())
                .await
                .expect("load")
                .expect("acceptance")
                .status(),
            PUBLISH_JOB_STATUS_FAILED
        );
        assert_eq!(
            fixture
                .count("ingress_effect_descendants WHERE settled_at IS NULL")
                .await,
            0
        );
        assert_eq!(
            fixture.count("ingress_effect_receipts").await,
            2,
            "terminal disposition adds no provider receipt"
        );
        assert_eq!(
            fixture
                .count("ingress_messages WHERE retention_eligible_at IS NOT NULL")
                .await,
            2
        );
        assert!(push
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts")
            .is_empty());
        let gc = gc_expired_aliases(
            &fixture.db,
            chrono::Utc::now() + chrono::Duration::days(9),
            AliasGcBudget {
                deadline: tokio::time::Instant::now() + std::time::Duration::from_secs(5),
                lock_timeout: std::time::Duration::from_millis(100),
                statement_timeout: std::time::Duration::from_secs(2),
                scan_timeout: std::time::Duration::from_secs(2),
                progress: Default::default(),
            },
        )
        .await
        .expect("retention GC");
        assert_eq!(gc.deleted_messages, 2);
        fixture.close().await;
    }

    #[tokio::test]
    async fn sqlite_missing_backing_cap_releases_all_canonical_parents() {
        missing_backing_cap_releases_all_canonical_parents(
            crate::ingress::test_support::IngressFixture::sqlite().await,
        )
        .await;
    }

    #[tokio::test]
    async fn postgres_missing_backing_cap_releases_all_canonical_parents() {
        if let Some(fixture) =
            crate::ingress::test_support::IngressFixture::postgres("push_backing_cap").await
        {
            missing_backing_cap_releases_all_canonical_parents(fixture).await;
        }
    }

    async fn postgres_parent_contention_preserves_batch_and_lease(attempt_count: i64, name: &str) {
        use crate::ingress_uow::{
            CanonicalMessageRepository, EffectDescendantRepository, EffectIntentRepository,
        };
        use waddle_xmpp::ingress::{
            IngressEffectIntent, MessageKey, NotificationActivityMutation,
            NotificationCandidateOutcome, SemanticDigest,
        };
        let Some(fixture) = crate::ingress::test_support::IngressFixture::postgres(name).await
        else {
            return;
        };
        crate::push_registrations::DatabasePushRegistrationStore::new(fixture.db.clone())
            .await
            .expect("registrations");
        let push = DatabasePushServiceStore::new_with_secret_key(
            fixture.db.clone(),
            &rand::random::<[u8; 32]>(),
        )
        .await
        .expect("store");
        let owner = owner();
        let service: BareJid = "push.example.com".parse().expect("service");
        let first = push
            .ensure_node(&owner, "contended")
            .await
            .expect("first node");
        let second = push
            .ensure_node(&owner, "unrelated")
            .await
            .expect("second node");
        for node in [first.node(), second.node()] {
            push.upsert_device(
                &owner,
                PushDeviceRegistration::new("device", node, PushDevicePlatform::Fcm, "test"),
            )
            .await
            .expect("device");
        }
        push.register_first_party_node_for_owner(&owner, service.as_str(), first.node(), None)
            .await
            .expect("registration");
        let delivery = uuid::Uuid::new_v4();
        let accepted = push
            .enqueue_canonical_notification_publish_job(
                first.node(),
                &notification_item("contended"),
                &owner,
                &service,
                None,
                delivery,
            )
            .await
            .expect("canonical acceptance");
        let unrelated = push
            .enqueue_notification_publish_job_from_user_server(
                second.node(),
                &notification_item("unrelated"),
                &owner,
            )
            .await
            .expect("unrelated acceptance");
        fixture.execute(
            "UPDATE push_publish_jobs SET created_at_ms = 1, attempt_count = ? WHERE job_id = ?",
            crate::db_params![attempt_count, accepted.job_id().to_string()],
        ).await;
        fixture
            .execute(
                "UPDATE push_publish_jobs SET created_at_ms = 2 WHERE job_id = ?",
                crate::db_params![unrelated.job_id().to_string()],
            )
            .await;
        let key = MessageKey::new();
        let intent = IngressEffectIntent::NotificationActivityPreview {
            owner: owner.clone(),
            mutation: NotificationActivityMutation::NotificationCandidate {
                conversation: "bob@example.com".parse().expect("conversation"),
                archive_stanza_id: waddle_xmpp_core::xep0359::StanzaId::new(
                    "contended",
                    owner.clone().into(),
                ),
                outcome: NotificationCandidateOutcome::Inserted,
            },
        };
        let mut tx = fixture.uow.begin().await.expect("parent tx");
        CanonicalMessageRepository::record_message(
            &mut tx,
            key,
            &SemanticDigest::from_storage(1, [1; 32]).expect("digest"),
            None,
        )
        .await
        .expect("parent");
        EffectIntentRepository::reconcile(&mut tx, key, std::slice::from_ref(&intent), false)
            .await
            .expect("intent");
        EffectDescendantRepository::attach(&mut tx, key, &intent.semantic_key(), delivery)
            .await
            .expect("custody");
        tx.commit().await.expect("commit parent");
        let mut held = fixture.uow.begin().await.expect("held parent tx");
        assert!(CanonicalMessageRepository::lock(&mut held, key)
            .await
            .expect("hold parent"));
        let results = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            push.drain_queued_notification_publish_jobs(2),
        )
        .await
        .expect("drain must not wait for the held parent")
        .expect("contention must not abort the batch");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].item_id(), "unrelated");
        let first_job = push
            .load_publish_job(&accepted.job_id().to_string())
            .await
            .expect("load")
            .expect("first job");
        let capped = attempt_count + 1 >= PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS;
        assert_eq!(
            first_job.status(),
            if capped {
                PUBLISH_JOB_STATUS_FAILED
            } else {
                PUBLISH_JOB_STATUS_QUEUED
            }
        );
        assert!(!first_job.uncertain_send);
        assert!(push
            .delivery_attempts_for_node(first.node())
            .await
            .expect("first attempts")
            .is_empty());
        assert_eq!(
            push.delivery_attempts_for_node(second.node())
                .await
                .expect("second attempts")
                .len(),
            1
        );
        assert_eq!(
            fixture
                .count("ingress_effect_descendants WHERE settled_at IS NULL")
                .await,
            1
        );
        assert_eq!(fixture.count("ingress_effect_receipts").await, 0);
        let mut rows = push
            .query(
                "SELECT attempt_count, next_retry_at_ms, updated_at_ms, last_error FROM push_publish_jobs WHERE job_id = ?",
                crate::db_params![accepted.job_id().to_string()],
            )
            .await
            .expect("retry");
        let row = rows.next().await.expect("row").expect("retry row");
        assert_eq!(row.get::<i64>(0).expect("count"), attempt_count + 1);
        assert!(row
            .get::<Option<i64>>(1)
            .expect("retry or probe timestamp")
            .is_some());
        let failure_updated_at = row.get::<i64>(2).expect("failure timestamp");
        let failure_error = row.get::<Option<String>>(3).expect("failure diagnostic");
        drop(rows);
        if capped {
            let accepted_id = accepted.job_id().to_string();
            assert!(
                push.queued_publish_jobs()
                    .await
                    .expect("provider queue")
                    .iter()
                    .all(|job| job.job_id() != accepted_id.as_str()),
                "a failed row's timestamp is a custody probe cursor, never a provider retry"
            );
            assert_eq!(
                tokio::time::timeout(
                    std::time::Duration::from_secs(2),
                    push.reconcile_failed_publish_job_custody(2)
                )
                .await
                .expect("custody probe must not wait for the held parent")
                .expect("deferred custody probe"),
                0
            );
            let mut rows = push.query("SELECT attempt_count, updated_at_ms, last_error FROM push_publish_jobs WHERE job_id = ?", crate::db_params![accepted.job_id().to_string()]).await.expect("failure history");
            let row = rows.next().await.expect("row").expect("failed job");
            assert_eq!(row.get::<i64>(0).expect("attempt count"), attempt_count + 1);
            assert_eq!(
                row.get::<i64>(1).expect("failure timestamp"),
                failure_updated_at,
                "probe rotation must preserve the original failure time"
            );
            assert_eq!(
                row.get::<Option<String>>(2).expect("failure diagnostic"),
                failure_error,
                "probe rotation must preserve the original failure diagnostic"
            );
            assert_eq!(
                fixture
                    .count("ingress_effect_descendants WHERE settled_at IS NULL")
                    .await,
                1,
                "the contended canonical parent retains custody"
            );
        }

        let claim = uuid::Uuid::new_v4().to_string();
        let claimed_at = crate::time::now_ms();
        fixture.execute(
            "UPDATE push_publish_jobs SET status = 'in-progress', claim_token = ?, claimed_at_ms = ?, attempt_count = 50, next_retry_at_ms = NULL WHERE job_id = ?",
            crate::db_params![claim.clone(), claimed_at, accepted.job_id().to_string()],
        ).await;
        tokio::time::timeout(
            std::time::Duration::from_secs(2),
            push.record_publish_job_failure_by_id(
                &accepted.job_id().to_string(),
                "late predecessor error",
            ),
        )
        .await
        .expect("repair must not wait for the held parent")
        .expect("repair owned work under contention");
        let repaired = push
            .load_publish_job(&accepted.job_id().to_string())
            .await
            .expect("load")
            .expect("owned job");
        assert_eq!(repaired.status(), PUBLISH_JOB_STATUS_IN_PROGRESS);
        assert_eq!(repaired.claim_token(), claim);
        assert!(repaired.uncertain_send);
        let mut rows = push.query(
            "SELECT attempt_count, claimed_at_ms, next_retry_at_ms FROM push_publish_jobs WHERE job_id = ?",
            crate::db_params![accepted.job_id().to_string()],
        ).await.expect("lease");
        let row = rows.next().await.expect("row").expect("lease row");
        assert_eq!(row.get::<i64>(0).expect("count"), 50);
        assert_eq!(row.get::<i64>(1).expect("claim time"), claimed_at);
        assert_eq!(row.get::<Option<i64>>(2).expect("retry deadline"), None);
        drop(rows);
        held.commit().await.expect("release parent");
        fixture.close().await;
    }

    #[tokio::test]
    async fn postgres_uncapped_parent_contention_preserves_batch_and_lease() {
        postgres_parent_contention_preserves_batch_and_lease(0, "push_uncapped_parent_lock").await;
    }

    #[tokio::test]
    async fn postgres_at_cap_parent_contention_preserves_batch_and_lease() {
        postgres_parent_contention_preserves_batch_and_lease(
            PUBLISH_JOB_MAX_TRANSIENT_ATTEMPTS - 1,
            "push_capped_parent_lock",
        )
        .await;
    }

    #[tokio::test]
    async fn expired_unknown_send_past_cap_is_parked_until_explicit_disposition() {
        let (store, _fcm) = store_with_fake_fcm().await;
        let owner = owner();
        let node = store
            .ensure_node(&owner, FCM_TEST_APP_ID)
            .await
            .expect("node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new(
                    "web-device",
                    node.node(),
                    PushDevicePlatform::Web,
                    "test",
                ),
            )
            .await
            .expect("device");
        let accepted = store
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("unknown-history"),
                &owner,
            )
            .await
            .expect("acceptance");
        store.execute("UPDATE push_publish_jobs SET status = 'in-progress', attempt_count = 50, claimed_at_ms = 1, claim_token = 'lost-claim' WHERE job_id = ?", crate::db_params![accepted.job_id().to_string()]).await.expect("lost expired send");
        store
            .process_publish_job_with_retention_limit(&accepted.job_id().to_string(), 10_000)
            .await
            .expect("known not-configured transient");
        assert!(store
            .queued_publish_jobs()
            .await
            .expect("queued")
            .is_empty());
        let parked = store
            .load_publish_job(&accepted.job_id().to_string())
            .await
            .expect("load")
            .expect("parked");
        assert_eq!(parked.status(), PUBLISH_JOB_STATUS_FAILED);
        assert!(
            parked.uncertain_send,
            "exhaustion parks the earlier unknown send"
        );
        // New healthy devices do not turn a parked record into fresh work.
        store
            .upsert_device(&owner, fcm_registration("known-device", node.node()))
            .await
            .expect("healthy device");
        assert!(store
            .process_publish_job_with_retention_limit(&accepted.job_id().to_string(), 10_000)
            .await
            .expect("parked retry")
            .is_none());
        assert_eq!(_fcm.sent(), 0, "parked uncertainty must not send again");
        let refused = store
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("unknown-refused"),
                &owner,
            )
            .await
            .expect("second acceptance");
        store.execute("UPDATE push_publish_jobs SET uncertain_send = 1, attempt_count = 50 WHERE job_id = ?", crate::db_params![refused.job_id().to_string()]).await.expect("prior uncertainty");
        store
            .disable_nodes_for_owner(&owner, Some(node.node()))
            .await
            .expect("explicit revocation");
        assert_eq!(
            store
                .load_publish_job(&refused.job_id().to_string())
                .await
                .expect("load")
                .expect("job")
                .status(),
            PUBLISH_JOB_STATUS_FAILED,
            "explicit revocation records refusal rather than fake provider success"
        );
    }

    #[tokio::test]
    async fn apns_badge_uses_live_account_unread_total() {
        use waddle_xmpp::inbox::{ConversationKind, InboxEntry};

        let recipient = owner();
        let inbox = Arc::new(waddle_xmpp::inbox::storage::InMemoryInboxStorage::new());
        for (partner, kind, thread_id, count) in [
            ("bob@example.com", ConversationKind::Direct, None, 2),
            ("carol@example.com", ConversationKind::Direct, None, 3),
            (
                "room@conference.example.com",
                ConversationKind::MucRoom,
                Some("thread-1"),
                5,
            ),
        ] {
            let partner: BareJid = partner.parse().expect("partner JID");
            for sequence in 0..count {
                let mut entry = InboxEntry::new(
                    partner.clone(),
                    kind,
                    format!("archive-{partner}-{sequence}"),
                    i64::from(sequence),
                );
                if let Some(thread_id) = thread_id {
                    entry = entry.with_thread(thread_id);
                }
                inbox
                    .upsert(&recipient, entry, true)
                    .await
                    .expect("upsert inbox entry");
            }
        }

        let store = store().await.with_inbox_storage(inbox);

        assert_eq!(store.current_apns_badge_count(&recipient).await, Some(5));
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
        assert!(attempt_status_warrants_device_disable(
            apns_dispatch::ATTEMPT_STATUS_APNS_GONE
        ));
        assert!(attempt_status_warrants_device_disable(
            apns_dispatch::ATTEMPT_STATUS_APNS_INVALID_TOKEN
        ));
        for disable in [
            fcm_dispatch::ATTEMPT_STATUS_FCM_GONE,
            fcm_dispatch::ATTEMPT_STATUS_FCM_INVALID_TOKEN,
            fcm_dispatch::ATTEMPT_STATUS_FCM_INVALID_KEYS,
        ] {
            assert!(
                attempt_status_warrants_device_disable(disable),
                "{disable} must disable the device"
            );
        }
        for never_disable in [
            dispatch::ATTEMPT_STATUS_WEB_DELIVERED,
            dispatch::ATTEMPT_STATUS_WEB_CLOCK_SKEW,
            dispatch::ATTEMPT_STATUS_WEB_RATE_LIMITED,
            dispatch::ATTEMPT_STATUS_WEB_TRANSIENT,
            dispatch::ATTEMPT_STATUS_WEB_BAD_REQUEST,
            dispatch::ATTEMPT_STATUS_WEB_PAYLOAD_TOO_LARGE,
            dispatch::ATTEMPT_STATUS_WEB_UNSEAL_FAILED,
            dispatch::ATTEMPT_STATUS_WEB_MISSING_MATERIAL,
            ATTEMPT_STATUS_WEB_NOT_CONFIGURED,
            ATTEMPT_STATUS_WEB_INTERNAL_ERROR,
            apns_dispatch::ATTEMPT_STATUS_APNS_DELIVERED,
            apns_dispatch::ATTEMPT_STATUS_APNS_TRANSIENT,
            apns_dispatch::ATTEMPT_STATUS_APNS_RATE_LIMITED,
            apns_dispatch::ATTEMPT_STATUS_APNS_PROVIDER_AUTH,
            apns_dispatch::ATTEMPT_STATUS_APNS_REJECTED,
            apns_dispatch::ATTEMPT_STATUS_APNS_PAYLOAD_TOO_LARGE,
            apns_dispatch::ATTEMPT_STATUS_APNS_TOPIC_MISMATCH,
            apns_dispatch::ATTEMPT_STATUS_APNS_NOT_CONFIGURED,
            apns_dispatch::ATTEMPT_STATUS_APNS_MISSING_TOKEN,
            apns_dispatch::ATTEMPT_STATUS_APNS_UNSEAL_FAILED,
            apns_dispatch::ATTEMPT_STATUS_APNS_INVALID_ENVIRONMENT,
            apns_dispatch::ATTEMPT_STATUS_APNS_INTERNAL_ERROR,
            fcm_dispatch::ATTEMPT_STATUS_FCM_DELIVERED,
            fcm_dispatch::ATTEMPT_STATUS_FCM_TRANSIENT,
            fcm_dispatch::ATTEMPT_STATUS_FCM_TOKEN_TRANSIENT,
            fcm_dispatch::ATTEMPT_STATUS_FCM_RATE_LIMITED,
            fcm_dispatch::ATTEMPT_STATUS_FCM_PROVIDER_AUTH,
            fcm_dispatch::ATTEMPT_STATUS_FCM_REJECTED,
            fcm_dispatch::ATTEMPT_STATUS_FCM_SENDER_ID_MISMATCH,
            fcm_dispatch::ATTEMPT_STATUS_FCM_PAYLOAD_TOO_LARGE,
            fcm_dispatch::ATTEMPT_STATUS_FCM_APP_ID_MISMATCH,
            fcm_dispatch::ATTEMPT_STATUS_FCM_NOT_CONFIGURED,
            fcm_dispatch::ATTEMPT_STATUS_FCM_MISSING_TOKEN,
            fcm_dispatch::ATTEMPT_STATUS_FCM_MISSING_KEYS,
            fcm_dispatch::ATTEMPT_STATUS_FCM_UNSEAL_FAILED,
            fcm_dispatch::ATTEMPT_STATUS_FCM_INTERNAL_ERROR,
        ] {
            assert!(
                !attempt_status_warrants_device_disable(never_disable),
                "{never_disable} must not trigger device disable"
            );
        }
    }

    #[test]
    fn uniform_provider_wide_failures_fail_the_job() {
        let attempt = |status: &'static str| DispatchedAttempt {
            device_id: "device".to_string(),
            platform: PushDevicePlatform::Fcm,
            status,
            last_error: None,
            retry_after: None,
            retry: None,
        };
        for uniform in [
            fcm_dispatch::ATTEMPT_STATUS_FCM_NOT_CONFIGURED,
            fcm_dispatch::ATTEMPT_STATUS_FCM_PROVIDER_AUTH,
            fcm_dispatch::ATTEMPT_STATUS_FCM_APP_ID_MISMATCH,
            fcm_dispatch::ATTEMPT_STATUS_FCM_SENDER_ID_MISMATCH,
            fcm_dispatch::ATTEMPT_STATUS_FCM_REJECTED,
            fcm_dispatch::ATTEMPT_STATUS_FCM_PAYLOAD_TOO_LARGE,
        ] {
            assert_eq!(
                all_attempts_with_encoder_bug_signature(&[attempt(uniform), attempt(uniform)]),
                Some(uniform)
            );
        }
        for per_device in [
            fcm_dispatch::ATTEMPT_STATUS_FCM_DELIVERED,
            fcm_dispatch::ATTEMPT_STATUS_FCM_GONE,
            fcm_dispatch::ATTEMPT_STATUS_FCM_INVALID_TOKEN,
            fcm_dispatch::ATTEMPT_STATUS_FCM_INVALID_KEYS,
            fcm_dispatch::ATTEMPT_STATUS_FCM_TRANSIENT,
            fcm_dispatch::ATTEMPT_STATUS_FCM_TOKEN_TRANSIENT,
            fcm_dispatch::ATTEMPT_STATUS_FCM_MISSING_TOKEN,
            fcm_dispatch::ATTEMPT_STATUS_FCM_UNSEAL_FAILED,
        ] {
            assert_eq!(
                all_attempts_with_encoder_bug_signature(&[attempt(per_device)]),
                None,
                "{per_device} is not a provider-wide failure"
            );
        }
    }

    // #529/#530: without `WADDLE_APNS_*` / `WADDLE_FCM_*`, Apple and
    // Android devices record the typed `*-not-configured` statuses, stay
    // active, and the job is not requeued.
    #[tokio::test]
    async fn unconfigured_apns_and_fcm_record_not_configured_and_keep_devices() {
        let store = store().await;
        let owner = owner();
        let node = store
            .ensure_node(&owner, "p4x.waddle.social")
            .await
            .expect("node");
        store
            .upsert_device(
                &owner,
                PushDeviceRegistration::new("ios-1", node.node(), PushDevicePlatform::Apns, "prod")
                    .with_provider_token(Some("abcdef0123".to_string())),
            )
            .await
            .expect("apple device");
        store
            .upsert_device(&owner, fcm_registration("android-1", node.node()))
            .await
            .expect("android device");
        store
            .enqueue_notification_publish_job_from_user_server(
                node.node(),
                &notification_item("unconfigured-apns"),
                &owner,
            )
            .await
            .expect("enqueue");

        store
            .drain_queued_notification_publish_jobs(16)
            .await
            .expect("drain");

        let attempts = store
            .delivery_attempts_for_node(node.node())
            .await
            .expect("attempts");
        let status_of = |device_id: &str| {
            attempts
                .iter()
                .find(|attempt| attempt.device_id() == device_id)
                .map(|attempt| attempt.status().to_string())
        };
        assert_eq!(
            status_of("ios-1").as_deref(),
            Some(apns_dispatch::ATTEMPT_STATUS_APNS_NOT_CONFIGURED)
        );
        assert_eq!(
            status_of("android-1").as_deref(),
            Some(fcm_dispatch::ATTEMPT_STATUS_FCM_NOT_CONFIGURED)
        );
        assert!(store
            .queued_publish_jobs()
            .await
            .expect("queued")
            .is_empty());
        for device_id in ["ios-1", "android-1"] {
            let mut rows = store
                .query(
                    "SELECT status FROM push_devices WHERE node = ? AND device_id = ?",
                    crate::db_params![node.node(), device_id],
                )
                .await
                .expect("device status");
            let row = rows.next().await.expect("row").expect("device row");
            assert_eq!(row.get::<String>(0).expect("status"), "active");
        }
    }

    #[tokio::test]
    async fn concurrent_publish_job_drains_claim_each_job_once() {
        let (store, fcm) = store_with_fake_fcm().await;
        let owner = owner();
        let node = store
            .ensure_node(&owner, FCM_TEST_APP_ID)
            .await
            .expect("node");
        store
            .upsert_device(&owner, fcm_registration("dev-1", node.node()))
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
        assert_eq!(
            attempts[0].status(),
            fcm_dispatch::ATTEMPT_STATUS_FCM_DELIVERED
        );
        assert_eq!(fcm.sent(), 1, "the job reached FCM exactly once");
        assert!(queued.is_empty());
    }

    #[tokio::test]
    async fn drain_continues_after_retryable_publish_job_failure() {
        let (store, _fcm) = store_with_fake_fcm().await;
        let owner = owner();
        let node = store
            .ensure_node(&owner, FCM_TEST_APP_ID)
            .await
            .expect("node");
        store
            .upsert_device(&owner, fcm_registration("dev-1", node.node()))
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
        assert!(queued.is_empty());
        let mut rows = store
            .query(
                "SELECT status, uncertain_send FROM push_publish_jobs WHERE item_id = ?",
                crate::db_params!["poison"],
            )
            .await
            .expect("failed phase3 lease");
        let row = rows.next().await.expect("row").expect("poison work");
        assert_eq!(
            row.get::<String>(0).expect("status"),
            PUBLISH_JOB_STATUS_IN_PROGRESS
        );
        assert_eq!(
            row.get::<i64>(1).expect("uncertainty"),
            1,
            "phase3 failure keeps the send unknown while unrelated queued work proceeds"
        );
    }

    #[tokio::test]
    async fn zero_device_retry_backoff_does_not_block_newer_jobs() {
        let (store, _fcm) = store_with_fake_fcm().await;
        let owner = owner();
        let zero_device_node = store.ensure_node(&owner, "web").await.expect("zero node");
        let live_node = store
            .ensure_node(&owner, FCM_TEST_APP_ID)
            .await
            .expect("live node");
        store
            .upsert_device(&owner, fcm_registration("android-1", live_node.node()))
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
