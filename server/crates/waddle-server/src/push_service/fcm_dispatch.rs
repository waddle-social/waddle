//! FCM dispatch bridge for the publish-job worker (#530).
//!
//! Mirrors the APNs bridge in [`super::apns_dispatch`]: it turns a sealed
//! `push_devices` row plus the parsed XEP-0357 payload into one typed
//! `waddle_xmpp::push::fcm` request, sends it, and maps the typed
//! [`FcmOutcome`] onto the `push_delivery_attempts.status` wire value.
//! It never touches the database; the worker records the result in
//! phase 3.
//!
//! The FCM message carries the same RFC 8291 `aes128gcm` ciphertext of
//! the v1 push envelope that Web Push delivers, encrypted to the
//! `p256dh`/`auth` keys the Android client registered, so Google never
//! sees the conversation, class or item.

use std::sync::Arc;
use std::time::Duration;

use waddle_xmpp::push::encrypt;
use waddle_xmpp::push::envelope::PushEnvelope;
use waddle_xmpp::push::fcm::{
    FcmAccessToken, FcmAccessTokenSource, FcmAppId, FcmMessage, FcmMessageError, FcmOutcome,
    FcmPriority, FcmProjectId, FcmRegistrationToken, FcmRequest, FcmSender, FcmTokenError,
    FcmTransient, FcmTtl,
};
use waddle_xmpp::push::types::{SubscriptionKeys, TransientFailure, WebPushCryptoError};
use waddle_xmpp::push::Urgency;
use waddle_xmpp::telemetry::attributes::MetricAttribute;

use super::dispatch::{envelope_item, ParsedPushPayload, SealedActiveDevice};
use super::secrets::PushSecretCipher;

pub(crate) const ATTEMPT_STATUS_FCM_DELIVERED: &str = "fcm-delivered";
/// FCM answered `UNREGISTERED`: the app was uninstalled or the token
/// expired. Disables the device (XEP-0357 §6).
pub(crate) const ATTEMPT_STATUS_FCM_GONE: &str = "fcm-gone";
/// FCM rejected the token itself (`INVALID_ARGUMENT` on `message.token`),
/// or the stored token is not a registration token. Disables the device.
pub(crate) const ATTEMPT_STATUS_FCM_INVALID_TOKEN: &str = "fcm-invalid-token";
/// The stored `p256dh`/`auth` keys do not decode: the ciphertext can never
/// be built for this device. Disables the device, like `web-invalid-keys`.
pub(crate) const ATTEMPT_STATUS_FCM_INVALID_KEYS: &str = "fcm-invalid-keys";
pub(crate) const ATTEMPT_STATUS_FCM_RATE_LIMITED: &str = "fcm-rate-limited";
/// Network failure, timeout or an FCM 5xx: FCM may have accepted the
/// message, so the retry is a possible duplicate.
pub(crate) const ATTEMPT_STATUS_FCM_TRANSIENT: &str = "fcm-transient";
/// The OAuth token endpoint was unreachable. FCM was never contacted, so
/// the retry cannot duplicate a delivery.
pub(crate) const ATTEMPT_STATUS_FCM_TOKEN_TRANSIENT: &str = "fcm-token-transient";
/// The server's credentials or project are wrong: the OAuth grant was
/// refused, FCM rejected a freshly minted access token, or FCM answered
/// `THIRD_PARTY_AUTH_ERROR` / `PERMISSION_DENIED` / a bare `403` / a
/// `404` without an FCM error code (unknown project). Never disables a
/// device.
pub(crate) const ATTEMPT_STATUS_FCM_PROVIDER_AUTH: &str = "fcm-provider-auth";
pub(crate) const ATTEMPT_STATUS_FCM_REJECTED: &str = "fcm-rejected";
/// The token belongs to another Firebase project than
/// `WADDLE_FCM_PROJECT_ID`.
pub(crate) const ATTEMPT_STATUS_FCM_SENDER_ID_MISMATCH: &str = "fcm-sender-id-mismatch";
pub(crate) const ATTEMPT_STATUS_FCM_PAYLOAD_TOO_LARGE: &str = "fcm-payload-too-large";
/// The node's `app_id` is not the Android application id this server
/// delivers for (`WADDLE_FCM_APP_ID`).
pub(crate) const ATTEMPT_STATUS_FCM_APP_ID_MISMATCH: &str = "fcm-app-id-mismatch";
/// No `WADDLE_FCM_*` configuration: Android devices cannot be reached.
/// Permanent (retrying cannot help until the operator redeploys) and
/// non-disabling (the device is fine).
pub(crate) const ATTEMPT_STATUS_FCM_NOT_CONFIGURED: &str = "fcm-not-configured";
pub(crate) const ATTEMPT_STATUS_FCM_MISSING_TOKEN: &str = "fcm-missing-token";
pub(crate) const ATTEMPT_STATUS_FCM_MISSING_KEYS: &str = "fcm-missing-keys";
pub(crate) const ATTEMPT_STATUS_FCM_UNSEAL_FAILED: &str = "fcm-unseal-failed";
pub(crate) const ATTEMPT_STATUS_FCM_INTERNAL_ERROR: &str = "fcm-internal-error";

/// Everything the worker needs to reach FCM, installed at boot by
/// [`super::DatabasePushServiceStore::with_fcm_provider`].
#[derive(Clone)]
pub(crate) struct FcmProvider {
    pub(crate) tokens: Arc<dyn FcmAccessTokenSource>,
    pub(crate) sender: Arc<dyn FcmSender>,
    pub(crate) project_id: FcmProjectId,
    /// The configured Android application id (`WADDLE_FCM_APP_ID`).
    pub(crate) app_id: FcmAppId,
}

impl FcmProvider {
    /// Whether a node registered for `app_id` may be sent to under this
    /// server's credentials.
    pub(super) fn serves_app_id(&self, app_id: &str) -> bool {
        self.app_id.as_str() == app_id
    }
}

/// Per-job values shared by every device of the fan-out.
#[derive(Debug, Clone, Copy)]
pub(super) struct FcmJobContext<'a> {
    pub(super) item_id: &'a str,
    /// `push_nodes.app_id`: the application the device registered with.
    pub(super) app_id: &'a str,
}

/// Typed result the worker wraps into its `DispatchedAttempt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct FcmAttempt {
    pub(super) status: &'static str,
    pub(super) last_error: Option<String>,
    pub(super) retry_after: Option<Duration>,
}

impl FcmAttempt {
    fn failed(status: &'static str, diagnostic: String) -> Self {
        Self {
            status,
            last_error: Some(diagnostic),
            retry_after: None,
        }
    }
}

/// A sealed Android device row, unsealed and validated.
struct FcmTarget {
    token: FcmRegistrationToken,
    keys: SubscriptionKeys,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FcmSkipReason {
    MissingToken,
    MissingKeys,
    UnsealFailed,
    InvalidToken,
    InvalidKeys,
}

impl FcmSkipReason {
    fn status(self) -> &'static str {
        match self {
            Self::MissingToken => ATTEMPT_STATUS_FCM_MISSING_TOKEN,
            Self::MissingKeys => ATTEMPT_STATUS_FCM_MISSING_KEYS,
            Self::UnsealFailed => ATTEMPT_STATUS_FCM_UNSEAL_FAILED,
            Self::InvalidToken => ATTEMPT_STATUS_FCM_INVALID_TOKEN,
            Self::InvalidKeys => ATTEMPT_STATUS_FCM_INVALID_KEYS,
        }
    }
}

impl FcmTarget {
    fn try_from_sealed(
        device: &SealedActiveDevice,
        cipher: &PushSecretCipher,
    ) -> Result<Self, FcmSkipReason> {
        let sealed_token = device
            .sealed_provider_token
            .as_ref()
            .ok_or(FcmSkipReason::MissingToken)?;
        let (Some(sealed_p256dh), Some(sealed_auth)) = (
            device.sealed_key_material.as_ref(),
            device.sealed_provider_auth.as_ref(),
        ) else {
            return Err(FcmSkipReason::MissingKeys);
        };
        let open = |sealed: &String| {
            cipher
                .open(sealed)
                .map(zeroize::Zeroizing::new)
                .map_err(|_| FcmSkipReason::UnsealFailed)
        };
        let token = open(sealed_token)?;
        let p256dh = open(sealed_p256dh)?;
        let auth = open(sealed_auth)?;
        let token = FcmRegistrationToken::parse(&token).map_err(|_| FcmSkipReason::InvalidToken)?;
        let keys = SubscriptionKeys::from_base64url(&p256dh, &auth)
            .map_err(|_| FcmSkipReason::InvalidKeys)?;
        Ok(Self { token, keys })
    }
}

/// Same urgency policy as Web Push and APNs: DMs and mentions wake the
/// device, `@everyone` broadcasts let Android defer them.
fn priority_for(urgency: Urgency) -> FcmPriority {
    match urgency {
        Urgency::High => FcmPriority::High,
        Urgency::Normal | Urgency::Low | Urgency::VeryLow => FcmPriority::Normal,
    }
}

fn log_skip(parsed: &ParsedPushPayload, status: &'static str) {
    tracing::warn!(
        notification_class = parsed.class.as_db_value(),
        provider = "fcm",
        push_stage = "provider_dispatch_skipped",
        provider_outcome = status,
        "push provider transition"
    );
}

/// Map a failed OAuth exchange to the attempt it produces. FCM was not
/// contacted, so nothing here is a possible duplicate delivery.
pub(super) fn token_error_attempt(error: &FcmTokenError) -> FcmAttempt {
    let status = match error {
        FcmTokenError::Transient(_) | FcmTokenError::MalformedResponse => {
            ATTEMPT_STATUS_FCM_TOKEN_TRANSIENT
        }
        FcmTokenError::Rejected { .. } => ATTEMPT_STATUS_FCM_PROVIDER_AUTH,
        FcmTokenError::Signing => ATTEMPT_STATUS_FCM_INTERNAL_ERROR,
    };
    let diagnostic = match error {
        FcmTokenError::Transient(TransientFailure::Network) => "oauth token: network".to_string(),
        FcmTokenError::Transient(TransientFailure::Timeout) => "oauth token: timeout".to_string(),
        FcmTokenError::Transient(TransientFailure::ServerError { status }) => {
            format!("oauth token: HTTP {status}")
        }
        FcmTokenError::MalformedResponse => "oauth token: malformed response".to_string(),
        FcmTokenError::Rejected { status, error } => {
            format!("oauth token refused: HTTP {status} {}", error.as_str())
        }
        FcmTokenError::Signing => "oauth token: assertion signing failed".to_string(),
    };
    FcmAttempt::failed(status, diagnostic)
}

/// Send one notification to one Android device. `access_token` is the
/// result of the single token fetch made before the fan-out; the worker
/// skips that fetch (`None`) only when the node's app id is not served,
/// which this function rejects first.
pub(super) async fn dispatch_fcm_device(
    device: &SealedActiveDevice,
    parsed: &ParsedPushPayload,
    job: FcmJobContext<'_>,
    provider: &FcmProvider,
    secrets: &PushSecretCipher,
    access_token: Option<&Result<FcmAccessToken, FcmTokenError>>,
) -> FcmAttempt {
    // The app id is checked before anything is unsealed or sent: a
    // registration for another app must never reach FCM under this
    // server's credentials.
    if !provider.serves_app_id(job.app_id) {
        log_skip(parsed, ATTEMPT_STATUS_FCM_APP_ID_MISMATCH);
        return FcmAttempt::failed(
            ATTEMPT_STATUS_FCM_APP_ID_MISMATCH,
            "push node app-id is not the configured FCM application id".to_string(),
        );
    }
    let target = match FcmTarget::try_from_sealed(device, secrets) {
        Ok(target) => target,
        Err(reason) => {
            log_skip(parsed, reason.status());
            return FcmAttempt {
                status: reason.status(),
                last_error: None,
                retry_after: None,
            };
        }
    };
    let policy = parsed.class.transport_policy();
    let conversation = parsed.conversation.to_string();
    let plaintext = zeroize::Zeroizing::new(
        PushEnvelope::new(
            parsed.class.as_db_value(),
            &conversation,
            parsed.thread.as_deref(),
            envelope_item(parsed, job.item_id),
            parsed.message_count,
        )
        .to_plaintext(),
    );
    let payload = match encrypt::encrypt(&target.keys, &plaintext, policy.bucket_size()) {
        Ok(payload) => payload,
        Err(error @ WebPushCryptoError::PayloadTooLarge { .. }) => {
            log_skip(parsed, ATTEMPT_STATUS_FCM_PAYLOAD_TOO_LARGE);
            return FcmAttempt::failed(ATTEMPT_STATUS_FCM_PAYLOAD_TOO_LARGE, error.to_string());
        }
        Err(error) => {
            log_skip(parsed, ATTEMPT_STATUS_FCM_INTERNAL_ERROR);
            return FcmAttempt::failed(
                ATTEMPT_STATUS_FCM_INTERNAL_ERROR,
                format!("envelope encryption failed: {error}"),
            );
        }
    };
    let message = match FcmMessage::new(
        target.token,
        &payload,
        priority_for(policy.urgency()),
        FcmTtl::new(Duration::from_secs(u64::from(policy.ttl()))),
    )
    .encode()
    {
        Ok(message) => message,
        Err(error @ FcmMessageError::TooLarge { .. }) => {
            log_skip(parsed, ATTEMPT_STATUS_FCM_PAYLOAD_TOO_LARGE);
            return FcmAttempt::failed(ATTEMPT_STATUS_FCM_PAYLOAD_TOO_LARGE, error.to_string());
        }
        Err(error @ FcmMessageError::Serialize(_)) => {
            log_skip(parsed, ATTEMPT_STATUS_FCM_INTERNAL_ERROR);
            return FcmAttempt::failed(ATTEMPT_STATUS_FCM_INTERNAL_ERROR, error.to_string());
        }
    };
    let token = match access_token {
        Some(Ok(token)) => token.clone(),
        Some(Err(error)) => {
            let attempt = token_error_attempt(error);
            log_skip(parsed, attempt.status);
            return attempt;
        }
        None => {
            log_skip(parsed, ATTEMPT_STATUS_FCM_INTERNAL_ERROR);
            return FcmAttempt::failed(
                ATTEMPT_STATUS_FCM_INTERNAL_ERROR,
                "no OAuth access token was fetched for this fan-out".to_string(),
            );
        }
    };
    let send = |token: FcmAccessToken| {
        let message = &message;
        async move {
            provider
                .sender
                .send(FcmRequest {
                    project_id: &provider.project_id,
                    access_token: &token,
                    message,
                })
                .await
        }
    };
    let mut outcome = send(token.clone()).await;
    record_outcome(&outcome, parsed);
    // FCM refused the access token: drop exactly the token it refused and
    // retry this device once with a fresh one. A second refusal means the
    // service account itself is not allowed to send, which is permanent.
    if matches!(outcome, FcmOutcome::Unauthenticated { .. }) {
        provider.tokens.invalidate(&token);
        let fresh = match provider.tokens.current().await {
            Ok(fresh) => fresh,
            Err(error) => {
                let attempt = token_error_attempt(&error);
                log_skip(parsed, attempt.status);
                return attempt;
            }
        };
        outcome = send(fresh).await;
        record_outcome(&outcome, parsed);
    }
    attempt_for_outcome(&outcome)
}

fn record_outcome(outcome: &FcmOutcome, parsed: &ParsedPushPayload) {
    let status = outcome_to_attempt_status(outcome);
    match waddle_xmpp::telemetry::push_pipeline::record_fcm_outcome(outcome) {
        Some(stage) => tracing::info!(
            notification_class = parsed.class.as_db_value(),
            provider = "fcm",
            push_stage = stage.value(),
            provider_outcome = status,
            "push provider transition"
        ),
        None => tracing::warn!(
            notification_class = parsed.class.as_db_value(),
            provider = "fcm",
            push_stage = "provider_no_response",
            provider_outcome = status,
            "push provider transition"
        ),
    }
}

/// Map a typed [`FcmOutcome`] to the persisted attempt status. An
/// `Unauthenticated` outcome only reaches here after the one retry with
/// a fresh token, so it is a provider-auth failure.
pub(super) fn outcome_to_attempt_status(outcome: &FcmOutcome) -> &'static str {
    match outcome {
        FcmOutcome::Sent => ATTEMPT_STATUS_FCM_DELIVERED,
        FcmOutcome::Unregistered { .. } => ATTEMPT_STATUS_FCM_GONE,
        FcmOutcome::InvalidToken { .. } => ATTEMPT_STATUS_FCM_INVALID_TOKEN,
        FcmOutcome::Unauthenticated { .. } | FcmOutcome::ProviderAuth { .. } => {
            ATTEMPT_STATUS_FCM_PROVIDER_AUTH
        }
        FcmOutcome::SenderIdMismatch { .. } => ATTEMPT_STATUS_FCM_SENDER_ID_MISMATCH,
        FcmOutcome::Transient {
            cause: FcmTransient::RateLimited,
            ..
        } => ATTEMPT_STATUS_FCM_RATE_LIMITED,
        FcmOutcome::Transient {
            cause: FcmTransient::Failure(_),
            ..
        } => ATTEMPT_STATUS_FCM_TRANSIENT,
        FcmOutcome::Rejected { .. } => ATTEMPT_STATUS_FCM_REJECTED,
    }
}

/// Operator diagnostic for `push_delivery_attempts.last_error`, built
/// only from the typed HTTP status and FCM error code (never Google's
/// free-text message).
fn outcome_diagnostic(outcome: &FcmOutcome) -> Option<String> {
    let diagnostic = match outcome {
        FcmOutcome::Sent => return None,
        FcmOutcome::Unregistered { status, detail } => {
            format!("unregistered HTTP {status} {detail}")
        }
        FcmOutcome::InvalidToken { status, detail } => {
            format!("invalid token HTTP {status} {detail}")
        }
        FcmOutcome::Unauthenticated { detail } => {
            format!("access token rejected after refresh HTTP 401 {detail}")
        }
        FcmOutcome::ProviderAuth { status, detail } => {
            format!("provider auth HTTP {status} {detail}")
        }
        FcmOutcome::SenderIdMismatch { status, detail } => {
            format!("sender id mismatch HTTP {status} {detail}")
        }
        FcmOutcome::Transient { cause, retry_after } => {
            let cause = match cause {
                FcmTransient::RateLimited => "rate limited".to_string(),
                FcmTransient::Failure(TransientFailure::ServerError { status }) => {
                    format!("transient: HTTP {status}")
                }
                FcmTransient::Failure(TransientFailure::Network) => {
                    "transient: network".to_string()
                }
                FcmTransient::Failure(TransientFailure::Timeout) => {
                    "transient: timeout".to_string()
                }
            };
            match retry_after {
                Some(delay) => format!("{cause} retry-after {}s", delay.as_secs()),
                None => cause,
            }
        }
        FcmOutcome::Rejected { status: 0, .. } => {
            "rejected (preflight: request could not be built)".to_string()
        }
        FcmOutcome::Rejected { status, detail } => {
            format!("rejected HTTP {status} {detail}")
        }
    };
    Some(diagnostic)
}

fn attempt_for_outcome(outcome: &FcmOutcome) -> FcmAttempt {
    let retry_after = match outcome {
        FcmOutcome::Transient { retry_after, .. } => *retry_after,
        _ => None,
    };
    FcmAttempt {
        status: outcome_to_attempt_status(outcome),
        last_error: outcome_diagnostic(outcome),
        retry_after,
    }
}

#[cfg(test)]
mod tests {
    use waddle_xmpp::push::fcm::{FcmErrorCode, FcmErrorDetail, GoogleRpcStatus, OAuthErrorCode};

    use super::*;

    fn detail(status: GoogleRpcStatus, code: Option<FcmErrorCode>) -> FcmErrorDetail {
        FcmErrorDetail {
            status: Some(status),
            error_code: code,
            token_field_violation: false,
        }
    }

    #[test]
    fn outcome_to_attempt_status_covers_every_variant() {
        for (outcome, expected) in [
            (FcmOutcome::Sent, ATTEMPT_STATUS_FCM_DELIVERED),
            (
                FcmOutcome::Unregistered {
                    status: 404,
                    detail: detail(GoogleRpcStatus::NotFound, Some(FcmErrorCode::Unregistered)),
                },
                ATTEMPT_STATUS_FCM_GONE,
            ),
            (
                FcmOutcome::InvalidToken {
                    status: 400,
                    detail: detail(GoogleRpcStatus::InvalidArgument, None),
                },
                ATTEMPT_STATUS_FCM_INVALID_TOKEN,
            ),
            (
                FcmOutcome::Unauthenticated {
                    detail: FcmErrorDetail::default(),
                },
                ATTEMPT_STATUS_FCM_PROVIDER_AUTH,
            ),
            (
                FcmOutcome::ProviderAuth {
                    status: 404,
                    detail: FcmErrorDetail::default(),
                },
                ATTEMPT_STATUS_FCM_PROVIDER_AUTH,
            ),
            (
                FcmOutcome::SenderIdMismatch {
                    status: 403,
                    detail: FcmErrorDetail::default(),
                },
                ATTEMPT_STATUS_FCM_SENDER_ID_MISMATCH,
            ),
            (
                FcmOutcome::Transient {
                    cause: FcmTransient::RateLimited,
                    retry_after: None,
                },
                ATTEMPT_STATUS_FCM_RATE_LIMITED,
            ),
            (
                FcmOutcome::Transient {
                    cause: FcmTransient::Failure(TransientFailure::Timeout),
                    retry_after: None,
                },
                ATTEMPT_STATUS_FCM_TRANSIENT,
            ),
            (
                FcmOutcome::Rejected {
                    status: 400,
                    detail: FcmErrorDetail::default(),
                },
                ATTEMPT_STATUS_FCM_REJECTED,
            ),
        ] {
            assert_eq!(outcome_to_attempt_status(&outcome), expected);
        }
    }

    #[test]
    fn diagnostics_carry_typed_codes_and_retry_after() {
        let attempt = attempt_for_outcome(&FcmOutcome::Transient {
            cause: FcmTransient::RateLimited,
            retry_after: Some(Duration::from_secs(90)),
        });
        assert_eq!(attempt.retry_after, Some(Duration::from_secs(90)));
        assert_eq!(
            attempt.last_error.as_deref(),
            Some("rate limited retry-after 90s")
        );
        let gone = attempt_for_outcome(&FcmOutcome::Unregistered {
            status: 404,
            detail: detail(GoogleRpcStatus::NotFound, Some(FcmErrorCode::Unregistered)),
        });
        assert_eq!(
            gone.last_error.as_deref(),
            Some("unregistered HTTP 404 NOT_FOUND/UNREGISTERED")
        );
        assert_eq!(gone.retry_after, None);
        let delivered = attempt_for_outcome(&FcmOutcome::Sent);
        assert_eq!(delivered.last_error, None);
    }

    #[test]
    fn token_errors_never_look_like_fcm_sends() {
        assert_eq!(
            token_error_attempt(&FcmTokenError::Transient(TransientFailure::Network)).status,
            ATTEMPT_STATUS_FCM_TOKEN_TRANSIENT
        );
        assert_eq!(
            token_error_attempt(&FcmTokenError::MalformedResponse).status,
            ATTEMPT_STATUS_FCM_TOKEN_TRANSIENT
        );
        let refused = token_error_attempt(&FcmTokenError::Rejected {
            status: 400,
            error: OAuthErrorCode::InvalidGrant,
        });
        assert_eq!(refused.status, ATTEMPT_STATUS_FCM_PROVIDER_AUTH);
        assert_eq!(
            refused.last_error.as_deref(),
            Some("oauth token refused: HTTP 400 invalid_grant")
        );
        assert_eq!(
            token_error_attempt(&FcmTokenError::Signing).status,
            ATTEMPT_STATUS_FCM_INTERNAL_ERROR
        );
    }

    #[test]
    fn notify_all_is_normal_priority_and_dms_are_high() {
        use waddle_xmpp::push::envelope::NotificationClass;
        assert_eq!(
            priority_for(NotificationClass::Dm.transport_policy().urgency()),
            FcmPriority::High
        );
        assert_eq!(
            priority_for(NotificationClass::NotifyAll.transport_policy().urgency()),
            FcmPriority::Normal
        );
    }
}
