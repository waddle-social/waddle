//! APNs dispatch bridge for the publish-job worker (#529).
//!
//! Mirrors the Web Push bridge in [`super::dispatch`]: it turns a sealed
//! `push_devices` row plus the parsed XEP-0357 payload into one typed
//! `waddle_xmpp::push::apns` request, sends it, and maps the typed
//! [`ApnsOutcome`] onto the `push_delivery_attempts.status` wire value.
//! It never touches the database; the worker records the result in
//! phase 3.

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use jid::BareJid;
use waddle_xmpp::push::apns::{
    ApnsCollapseId, ApnsDeviceToken, ApnsEnvironment, ApnsExpiration, ApnsOutcome, ApnsPayload,
    ApnsPayloadError, ApnsPayloadFields, ApnsPriority, ApnsProviderJwt, ApnsProviderTokenSource,
    ApnsRequest, ApnsSender, ApnsTopic, ApnsTransient,
};
use waddle_xmpp::push::types::TransientFailure;
use waddle_xmpp::push::Urgency;
use waddle_xmpp::telemetry::attributes::MetricAttribute;

use super::commands::PushDeviceEnvironment;
use super::dispatch::{envelope_item, ParsedPushPayload, SealedActiveDevice};
use super::secrets::PushSecretCipher;

pub(crate) const ATTEMPT_STATUS_APNS_DELIVERED: &str = "apns-delivered";
/// Token is dead (410, `BadDeviceToken`, `DeviceTokenNotForTopic`).
/// The only APNs status that disables the device (XEP-0357 §6).
pub(crate) const ATTEMPT_STATUS_APNS_GONE: &str = "apns-gone";
pub(crate) const ATTEMPT_STATUS_APNS_RATE_LIMITED: &str = "apns-rate-limited";
pub(crate) const ATTEMPT_STATUS_APNS_TRANSIENT: &str = "apns-transient";
/// Provider token rejected again after a refresh, or rejected while too
/// young to refresh (Apple's 20-minute floor): the `.p8` key, key id, team
/// id or server clock is wrong.
pub(crate) const ATTEMPT_STATUS_APNS_PROVIDER_AUTH: &str = "apns-provider-auth";
pub(crate) const ATTEMPT_STATUS_APNS_REJECTED: &str = "apns-rejected";
pub(crate) const ATTEMPT_STATUS_APNS_PAYLOAD_TOO_LARGE: &str = "apns-payload-too-large";
/// The node's `app_id` is not the bundle id this server signs for.
pub(crate) const ATTEMPT_STATUS_APNS_TOPIC_MISMATCH: &str = "apns-topic-mismatch";
/// No `WADDLE_APNS_*` configuration: Apple devices cannot be reached.
/// Permanent (retrying cannot help until the operator redeploys) and
/// non-disabling (the device is fine).
pub(crate) const ATTEMPT_STATUS_APNS_NOT_CONFIGURED: &str = "apns-not-configured";
pub(crate) const ATTEMPT_STATUS_APNS_MISSING_TOKEN: &str = "apns-missing-token";
pub(crate) const ATTEMPT_STATUS_APNS_UNSEAL_FAILED: &str = "apns-unseal-failed";
/// Stored token is not hex: structurally unusable, disables the device.
pub(crate) const ATTEMPT_STATUS_APNS_INVALID_TOKEN: &str = "apns-invalid-token";
pub(crate) const ATTEMPT_STATUS_APNS_INVALID_ENVIRONMENT: &str = "apns-invalid-environment";
pub(crate) const ATTEMPT_STATUS_APNS_INTERNAL_ERROR: &str = "apns-internal-error";

/// Everything the worker needs to reach APNs, installed at boot by
/// [`super::DatabasePushServiceStore::with_apns_provider`].
#[derive(Clone)]
pub(crate) struct ApnsProvider {
    pub(crate) tokens: Arc<dyn ApnsProviderTokenSource>,
    pub(crate) sender: Arc<dyn ApnsSender>,
    /// The configured bundle id (`WADDLE_APNS_BUNDLE_ID`).
    pub(crate) topic: ApnsTopic,
}

/// Per-job values shared by every device of the fan-out.
#[derive(Debug, Clone, Copy)]
pub(super) struct ApnsJobContext<'a> {
    pub(super) item_id: &'a str,
    pub(super) node: &'a str,
    /// `push_nodes.app_id`: the bundle id the device registered with.
    pub(super) app_id: &'a str,
}

/// Typed result the worker wraps into its `DispatchedAttempt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ApnsAttempt {
    pub(super) status: &'static str,
    pub(super) last_error: Option<String>,
    pub(super) retry_after: Option<Duration>,
}

impl ApnsAttempt {
    fn failed(status: &'static str, diagnostic: String) -> Self {
        Self {
            status,
            last_error: Some(diagnostic),
            retry_after: None,
        }
    }
}

/// A sealed Apple device row, unsealed and validated.
struct ApnsTarget {
    environment: ApnsEnvironment,
    token: ApnsDeviceToken,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ApnsSkipReason {
    MissingToken,
    UnsealFailed,
    InvalidToken,
    InvalidEnvironment,
}

impl ApnsSkipReason {
    fn status(self) -> &'static str {
        match self {
            Self::MissingToken => ATTEMPT_STATUS_APNS_MISSING_TOKEN,
            Self::UnsealFailed => ATTEMPT_STATUS_APNS_UNSEAL_FAILED,
            Self::InvalidToken => ATTEMPT_STATUS_APNS_INVALID_TOKEN,
            Self::InvalidEnvironment => ATTEMPT_STATUS_APNS_INVALID_ENVIRONMENT,
        }
    }
}

impl ApnsTarget {
    fn try_from_sealed(
        device: &SealedActiveDevice,
        cipher: &PushSecretCipher,
    ) -> Result<Self, ApnsSkipReason> {
        let environment = match device.environment {
            Some(PushDeviceEnvironment::Production) => ApnsEnvironment::Production,
            Some(PushDeviceEnvironment::Sandbox) => ApnsEnvironment::Sandbox,
            None => return Err(ApnsSkipReason::InvalidEnvironment),
        };
        let sealed = device
            .sealed_provider_token
            .as_ref()
            .ok_or(ApnsSkipReason::MissingToken)?;
        let plain = cipher
            .open(sealed)
            .map_err(|_| ApnsSkipReason::UnsealFailed)?;
        let token = ApnsDeviceToken::parse(&plain).map_err(|_| ApnsSkipReason::InvalidToken)?;
        Ok(Self { environment, token })
    }
}

fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_secs())
        .unwrap_or(0)
}

/// Same urgency policy as Web Push: DMs and mentions wake the device,
/// `@everyone` broadcasts let it batch.
fn priority_for(urgency: Urgency) -> ApnsPriority {
    match urgency {
        Urgency::High => ApnsPriority::Immediate,
        Urgency::Normal | Urgency::Low | Urgency::VeryLow => ApnsPriority::PowerConsiderate,
    }
}

/// Send one notification to one Apple device.
pub(super) async fn dispatch_apns_device(
    device: &SealedActiveDevice,
    recipient: &BareJid,
    parsed: &ParsedPushPayload,
    job: ApnsJobContext<'_>,
    provider: &ApnsProvider,
    secrets: &PushSecretCipher,
) -> ApnsAttempt {
    let log_skip = |status: &'static str| {
        tracing::warn!(
            recipient = %recipient,
            conversation = %parsed.conversation,
            notification_class = parsed.class.as_db_value(),
            provider = "apns",
            push_stage = "provider_dispatch_skipped",
            provider_outcome = status,
            "push provider transition"
        );
    };
    // The topic is checked before anything is unsealed or signed: a
    // registration for another app must never reach Apple under our
    // provider token.
    if provider.topic.as_str() != job.app_id {
        log_skip(ATTEMPT_STATUS_APNS_TOPIC_MISMATCH);
        return ApnsAttempt::failed(
            ATTEMPT_STATUS_APNS_TOPIC_MISMATCH,
            "push node app-id is not the configured APNs bundle id".to_string(),
        );
    }
    let target = match ApnsTarget::try_from_sealed(device, secrets) {
        Ok(target) => target,
        Err(reason) => {
            log_skip(reason.status());
            return ApnsAttempt {
                status: reason.status(),
                last_error: None,
                retry_after: None,
            };
        }
    };
    let conversation = parsed.conversation.to_string();
    let item = envelope_item(parsed, job.item_id);
    let encoded = match ApnsPayload::new(ApnsPayloadFields {
        class: parsed.class,
        conversation: &conversation,
        thread: parsed.thread.as_deref(),
        item,
        node: job.node,
        message_count: parsed.message_count,
    })
    .encode()
    {
        Ok(encoded) => encoded,
        Err(error @ ApnsPayloadError::TooLarge { .. }) => {
            log_skip(ATTEMPT_STATUS_APNS_PAYLOAD_TOO_LARGE);
            return ApnsAttempt::failed(ATTEMPT_STATUS_APNS_PAYLOAD_TOO_LARGE, error.to_string());
        }
        Err(error @ ApnsPayloadError::Serialize(_)) => {
            log_skip(ATTEMPT_STATUS_APNS_INTERNAL_ERROR);
            return ApnsAttempt::failed(ATTEMPT_STATUS_APNS_INTERNAL_ERROR, error.to_string());
        }
    };
    let policy = parsed.class.transport_policy();
    let collapse_id = ApnsCollapseId::new(item);
    let send = |jwt: ApnsProviderJwt| {
        let token = &target.token;
        let encoded = &encoded;
        let collapse_id = collapse_id.as_ref();
        async move {
            provider
                .sender
                .send(ApnsRequest {
                    environment: target.environment,
                    device_token: token,
                    topic: &provider.topic,
                    provider_token: &jwt,
                    payload: encoded,
                    priority: priority_for(policy.urgency()),
                    expiration: ApnsExpiration::after(
                        now_unix_seconds(),
                        Duration::from_secs(u64::from(policy.ttl())),
                    ),
                    collapse_id,
                })
                .await
        }
    };
    let jwt = match provider.tokens.current() {
        Ok(jwt) => jwt,
        Err(error) => {
            log_skip(ATTEMPT_STATUS_APNS_INTERNAL_ERROR);
            return ApnsAttempt::failed(ATTEMPT_STATUS_APNS_INTERNAL_ERROR, error.to_string());
        }
    };
    let mut outcome = send(jwt.clone()).await;
    record_outcome(&outcome, recipient, parsed);
    // Expired or rejected provider token: drop exactly the token Apple
    // refused and retry this device once with a fresh one. A token too
    // young to refresh, or a second refusal, is a key/team/key-id
    // misconfiguration and is recorded as permanent.
    if matches!(outcome, ApnsOutcome::ProviderAuth { .. }) && provider.tokens.invalidate(&jwt) {
        let fresh = match provider.tokens.current() {
            Ok(fresh) => fresh,
            Err(error) => {
                log_skip(ATTEMPT_STATUS_APNS_INTERNAL_ERROR);
                return ApnsAttempt::failed(ATTEMPT_STATUS_APNS_INTERNAL_ERROR, error.to_string());
            }
        };
        outcome = send(fresh).await;
        record_outcome(&outcome, recipient, parsed);
    }
    attempt_for_outcome(&outcome)
}

fn record_outcome(outcome: &ApnsOutcome, recipient: &BareJid, parsed: &ParsedPushPayload) {
    let status = outcome_to_attempt_status(outcome);
    match waddle_xmpp::telemetry::push_pipeline::record_apns_outcome(outcome) {
        Some(stage) => tracing::info!(
            recipient = %recipient,
            conversation = %parsed.conversation,
            notification_class = parsed.class.as_db_value(),
            provider = "apns",
            push_stage = stage.value(),
            provider_outcome = status,
            "push provider transition"
        ),
        None => tracing::warn!(
            recipient = %recipient,
            conversation = %parsed.conversation,
            notification_class = parsed.class.as_db_value(),
            provider = "apns",
            push_stage = "provider_no_response",
            provider_outcome = status,
            "push provider transition"
        ),
    }
}

/// Map a typed [`ApnsOutcome`] to the persisted attempt status.
pub(super) fn outcome_to_attempt_status(outcome: &ApnsOutcome) -> &'static str {
    match outcome {
        ApnsOutcome::Sent { .. } => ATTEMPT_STATUS_APNS_DELIVERED,
        ApnsOutcome::DeviceGone { .. } => ATTEMPT_STATUS_APNS_GONE,
        ApnsOutcome::ProviderAuth { .. } => ATTEMPT_STATUS_APNS_PROVIDER_AUTH,
        ApnsOutcome::Transient {
            cause: ApnsTransient::RateLimited { .. },
            ..
        } => ATTEMPT_STATUS_APNS_RATE_LIMITED,
        ApnsOutcome::Transient {
            cause: ApnsTransient::Failure(_),
            ..
        } => ATTEMPT_STATUS_APNS_TRANSIENT,
        ApnsOutcome::Rejected { .. } => ATTEMPT_STATUS_APNS_REJECTED,
    }
}

fn outcome_diagnostic(outcome: &ApnsOutcome) -> Option<String> {
    let diagnostic = match outcome {
        ApnsOutcome::Sent { .. } => return None,
        ApnsOutcome::DeviceGone { status, reason } => {
            format!("device gone HTTP {status} {}", reason.as_str())
        }
        ApnsOutcome::ProviderAuth { reason } => {
            format!("provider token rejected: {}", reason.as_str())
        }
        ApnsOutcome::Transient { cause, retry_after } => {
            let cause = match cause {
                ApnsTransient::RateLimited { reason } => {
                    format!("rate limited HTTP 429 {}", reason.as_str())
                }
                ApnsTransient::Failure(TransientFailure::ServerError { status }) => {
                    format!("transient: HTTP {status}")
                }
                ApnsTransient::Failure(TransientFailure::Network) => {
                    "transient: network".to_string()
                }
                ApnsTransient::Failure(TransientFailure::Timeout) => {
                    "transient: timeout".to_string()
                }
            };
            match retry_after {
                Some(delay) => format!("{cause} retry-after {}s", delay.as_secs()),
                None => cause,
            }
        }
        ApnsOutcome::Rejected { status: 0, .. } => {
            "rejected (preflight: request could not be built)".to_string()
        }
        ApnsOutcome::Rejected { status, reason } => {
            format!("rejected HTTP {status} {}", reason.as_str())
        }
    };
    Some(diagnostic)
}

fn attempt_for_outcome(outcome: &ApnsOutcome) -> ApnsAttempt {
    let retry_after = match outcome {
        ApnsOutcome::Transient { retry_after, .. } => *retry_after,
        _ => None,
    };
    ApnsAttempt {
        status: outcome_to_attempt_status(outcome),
        last_error: outcome_diagnostic(outcome),
        retry_after,
    }
}

#[cfg(test)]
mod tests {
    use waddle_xmpp::push::apns::ApnsReason;

    use super::*;

    #[test]
    fn outcome_to_attempt_status_covers_every_variant() {
        for (outcome, expected) in [
            (
                ApnsOutcome::Sent { apns_id: None },
                ATTEMPT_STATUS_APNS_DELIVERED,
            ),
            (
                ApnsOutcome::DeviceGone {
                    status: 410,
                    reason: ApnsReason::Unregistered,
                },
                ATTEMPT_STATUS_APNS_GONE,
            ),
            (
                ApnsOutcome::ProviderAuth {
                    reason: ApnsReason::InvalidProviderToken,
                },
                ATTEMPT_STATUS_APNS_PROVIDER_AUTH,
            ),
            (
                ApnsOutcome::Transient {
                    cause: ApnsTransient::RateLimited {
                        reason: ApnsReason::TooManyRequests,
                    },
                    retry_after: None,
                },
                ATTEMPT_STATUS_APNS_RATE_LIMITED,
            ),
            (
                ApnsOutcome::Transient {
                    cause: ApnsTransient::Failure(TransientFailure::Timeout),
                    retry_after: None,
                },
                ATTEMPT_STATUS_APNS_TRANSIENT,
            ),
            (
                ApnsOutcome::Rejected {
                    status: 413,
                    reason: ApnsReason::PayloadTooLarge,
                },
                ATTEMPT_STATUS_APNS_REJECTED,
            ),
        ] {
            assert_eq!(outcome_to_attempt_status(&outcome), expected);
        }
    }

    #[test]
    fn retry_after_is_carried_only_for_transient_outcomes() {
        let attempt = attempt_for_outcome(&ApnsOutcome::Transient {
            cause: ApnsTransient::RateLimited {
                reason: ApnsReason::TooManyRequests,
            },
            retry_after: Some(Duration::from_secs(90)),
        });
        assert_eq!(attempt.retry_after, Some(Duration::from_secs(90)));
        assert_eq!(
            attempt.last_error.as_deref(),
            Some("rate limited HTTP 429 TooManyRequests retry-after 90s")
        );
        let delivered = attempt_for_outcome(&ApnsOutcome::Sent { apns_id: None });
        assert_eq!(delivered.retry_after, None);
        assert_eq!(delivered.last_error, None);
    }

    #[test]
    fn notify_all_is_power_considerate_and_dms_are_immediate() {
        use waddle_xmpp::push::envelope::NotificationClass;
        assert_eq!(
            priority_for(NotificationClass::Dm.transport_policy().urgency()),
            ApnsPriority::Immediate
        );
        assert_eq!(
            priority_for(
                NotificationClass::PersonalMention
                    .transport_policy()
                    .urgency()
            ),
            ApnsPriority::Immediate
        );
        assert_eq!(
            priority_for(NotificationClass::NotifyAll.transport_policy().urgency()),
            ApnsPriority::PowerConsiderate
        );
    }
}
