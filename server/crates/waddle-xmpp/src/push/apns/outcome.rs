//! Typed APNs delivery outcomes and Apple's documented rejection
//! reasons ("Handling notification responses from APNs").

use std::time::Duration;

use serde::Deserialize;

use crate::push::types::TransientFailure;

/// The `reason` string of an APNs error response body
/// (`{"reason":"BadDeviceToken"}`), parsed into a closed set. Reasons
/// Apple adds later deserialize to [`ApnsReason::Unrecognized`], which
/// never triggers a device disable: only the documented token-death
/// reasons do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
pub enum ApnsReason {
    BadCollapseId,
    BadDeviceToken,
    BadExpirationDate,
    BadMessageId,
    BadPriority,
    BadTopic,
    DeviceTokenNotForTopic,
    DuplicateHeaders,
    IdleTimeout,
    InvalidPushType,
    MissingDeviceToken,
    MissingTopic,
    PayloadEmpty,
    TopicDisallowed,
    BadCertificate,
    BadCertificateEnvironment,
    ExpiredProviderToken,
    Forbidden,
    InvalidProviderToken,
    MissingProviderToken,
    UnrelatedKeyIdInToken,
    BadEnvironmentKeyInToken,
    BadPath,
    MethodNotAllowed,
    ExpiredToken,
    Unregistered,
    PayloadTooLarge,
    TooManyProviderTokenUpdates,
    TooManyRequests,
    InternalServerError,
    ServiceUnavailable,
    Shutdown,
    /// No body, a body that is not Apple's JSON shape, or a reason this
    /// build does not know.
    #[serde(other)]
    Unrecognized,
}

impl ApnsReason {
    /// Apple's wire spelling, for operator diagnostics only.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::BadCollapseId => "BadCollapseId",
            Self::BadDeviceToken => "BadDeviceToken",
            Self::BadExpirationDate => "BadExpirationDate",
            Self::BadMessageId => "BadMessageId",
            Self::BadPriority => "BadPriority",
            Self::BadTopic => "BadTopic",
            Self::DeviceTokenNotForTopic => "DeviceTokenNotForTopic",
            Self::DuplicateHeaders => "DuplicateHeaders",
            Self::IdleTimeout => "IdleTimeout",
            Self::InvalidPushType => "InvalidPushType",
            Self::MissingDeviceToken => "MissingDeviceToken",
            Self::MissingTopic => "MissingTopic",
            Self::PayloadEmpty => "PayloadEmpty",
            Self::TopicDisallowed => "TopicDisallowed",
            Self::BadCertificate => "BadCertificate",
            Self::BadCertificateEnvironment => "BadCertificateEnvironment",
            Self::ExpiredProviderToken => "ExpiredProviderToken",
            Self::Forbidden => "Forbidden",
            Self::InvalidProviderToken => "InvalidProviderToken",
            Self::MissingProviderToken => "MissingProviderToken",
            Self::UnrelatedKeyIdInToken => "UnrelatedKeyIdInToken",
            Self::BadEnvironmentKeyInToken => "BadEnvironmentKeyInToken",
            Self::BadPath => "BadPath",
            Self::MethodNotAllowed => "MethodNotAllowed",
            Self::ExpiredToken => "ExpiredToken",
            Self::Unregistered => "Unregistered",
            Self::PayloadTooLarge => "PayloadTooLarge",
            Self::TooManyProviderTokenUpdates => "TooManyProviderTokenUpdates",
            Self::TooManyRequests => "TooManyRequests",
            Self::InternalServerError => "InternalServerError",
            Self::ServiceUnavailable => "ServiceUnavailable",
            Self::Shutdown => "Shutdown",
            Self::Unrecognized => "unrecognized",
        }
    }

    /// Parse an APNs error response body. Anything that is not
    /// `{"reason": <known>}` is [`ApnsReason::Unrecognized`].
    pub fn from_body(body: &[u8]) -> Self {
        #[derive(Deserialize)]
        struct ErrorBody {
            reason: ApnsReason,
        }
        serde_json::from_slice::<ErrorBody>(body)
            .map(|parsed| parsed.reason)
            .unwrap_or(Self::Unrecognized)
    }
}

/// The `apns-id` Apple echoes on every response (a canonical UUID).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ApnsId(uuid::Uuid);

impl ApnsId {
    pub fn parse(value: &str) -> Option<Self> {
        uuid::Uuid::parse_str(value).ok().map(Self)
    }

    pub fn as_uuid(&self) -> uuid::Uuid {
        self.0
    }
}

/// Why a send is worth retrying later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApnsTransient {
    /// `429` — `TooManyRequests` for this device or
    /// `TooManyProviderTokenUpdates` for this provider.
    RateLimited { reason: ApnsReason },
    /// Network error, timeout, or an APNs 5xx.
    Failure(TransientFailure),
}

/// Typed result of one APNs send. Every failure mode is a variant; the
/// sender never returns `Result`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApnsOutcome {
    /// `200`: APNs accepted the notification.
    Sent { apns_id: Option<ApnsId> },
    /// The device token is permanently unusable: `410` (`Unregistered`,
    /// `ExpiredToken`), or `400` `BadDeviceToken` /
    /// `DeviceTokenNotForTopic`. Only this outcome disables the device.
    DeviceGone { status: u16, reason: ApnsReason },
    /// `403` `ExpiredProviderToken` / `InvalidProviderToken`: our
    /// provider JWT was rejected. The caller invalidates the cached
    /// token and retries once with a fresh one.
    ProviderAuth { reason: ApnsReason },
    /// Worth retrying: `429`, `5xx`, network failure or timeout.
    Transient {
        cause: ApnsTransient,
        retry_after: Option<Duration>,
    },
    /// Any other rejection. Permanent for this notification; the device
    /// is kept because the fault is not the token's.
    Rejected { status: u16, reason: ApnsReason },
}

/// Map an APNs HTTP response to its typed outcome. Pure so the whole
/// table is unit-testable without a server.
pub fn classify_response(
    status: u16,
    reason: ApnsReason,
    retry_after: Option<Duration>,
    apns_id: Option<ApnsId>,
) -> ApnsOutcome {
    match (status, reason) {
        (200, _) => ApnsOutcome::Sent { apns_id },
        (410, _) | (400, ApnsReason::BadDeviceToken | ApnsReason::DeviceTokenNotForTopic) => {
            ApnsOutcome::DeviceGone { status, reason }
        }
        (403, ApnsReason::ExpiredProviderToken | ApnsReason::InvalidProviderToken) => {
            ApnsOutcome::ProviderAuth { reason }
        }
        (429, _) => ApnsOutcome::Transient {
            cause: ApnsTransient::RateLimited { reason },
            retry_after,
        },
        (500..=599, _) => ApnsOutcome::Transient {
            cause: ApnsTransient::Failure(TransientFailure::ServerError { status }),
            retry_after,
        },
        _ => ApnsOutcome::Rejected { status, reason },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const APNS_ID: &str = "8c8b2a84-7c8e-4a8f-9a0e-1c1f5d8e2b3a";

    fn reason(body: &str) -> ApnsReason {
        ApnsReason::from_body(body.as_bytes())
    }

    #[test]
    fn body_parses_known_reasons_and_folds_the_rest() {
        assert_eq!(
            reason(r#"{"reason":"BadDeviceToken"}"#),
            ApnsReason::BadDeviceToken
        );
        assert_eq!(
            reason(r#"{"reason":"Unregistered","timestamp":1700000000000}"#),
            ApnsReason::Unregistered
        );
        assert_eq!(
            reason(r#"{"reason":"SomethingNew"}"#),
            ApnsReason::Unrecognized
        );
        assert_eq!(reason(""), ApnsReason::Unrecognized);
        assert_eq!(reason("<html>"), ApnsReason::Unrecognized);
    }

    #[test]
    fn response_mapping_table() {
        let id = ApnsId::parse(APNS_ID);
        let cases = [
            (
                200,
                ApnsReason::Unrecognized,
                None,
                ApnsOutcome::Sent { apns_id: id },
            ),
            (
                400,
                ApnsReason::BadDeviceToken,
                None,
                ApnsOutcome::DeviceGone {
                    status: 400,
                    reason: ApnsReason::BadDeviceToken,
                },
            ),
            (
                400,
                ApnsReason::DeviceTokenNotForTopic,
                None,
                ApnsOutcome::DeviceGone {
                    status: 400,
                    reason: ApnsReason::DeviceTokenNotForTopic,
                },
            ),
            (
                400,
                ApnsReason::BadPriority,
                None,
                ApnsOutcome::Rejected {
                    status: 400,
                    reason: ApnsReason::BadPriority,
                },
            ),
            (
                403,
                ApnsReason::ExpiredProviderToken,
                None,
                ApnsOutcome::ProviderAuth {
                    reason: ApnsReason::ExpiredProviderToken,
                },
            ),
            (
                403,
                ApnsReason::InvalidProviderToken,
                None,
                ApnsOutcome::ProviderAuth {
                    reason: ApnsReason::InvalidProviderToken,
                },
            ),
            (
                403,
                ApnsReason::UnrelatedKeyIdInToken,
                None,
                ApnsOutcome::Rejected {
                    status: 403,
                    reason: ApnsReason::UnrelatedKeyIdInToken,
                },
            ),
            (
                410,
                ApnsReason::Unregistered,
                None,
                ApnsOutcome::DeviceGone {
                    status: 410,
                    reason: ApnsReason::Unregistered,
                },
            ),
            (
                410,
                ApnsReason::ExpiredToken,
                None,
                ApnsOutcome::DeviceGone {
                    status: 410,
                    reason: ApnsReason::ExpiredToken,
                },
            ),
            (
                413,
                ApnsReason::PayloadTooLarge,
                None,
                ApnsOutcome::Rejected {
                    status: 413,
                    reason: ApnsReason::PayloadTooLarge,
                },
            ),
            (
                429,
                ApnsReason::TooManyRequests,
                Some(Duration::from_secs(30)),
                ApnsOutcome::Transient {
                    cause: ApnsTransient::RateLimited {
                        reason: ApnsReason::TooManyRequests,
                    },
                    retry_after: Some(Duration::from_secs(30)),
                },
            ),
            (
                500,
                ApnsReason::InternalServerError,
                None,
                ApnsOutcome::Transient {
                    cause: ApnsTransient::Failure(TransientFailure::ServerError { status: 500 }),
                    retry_after: None,
                },
            ),
            (
                503,
                ApnsReason::ServiceUnavailable,
                Some(Duration::from_secs(5)),
                ApnsOutcome::Transient {
                    cause: ApnsTransient::Failure(TransientFailure::ServerError { status: 503 }),
                    retry_after: Some(Duration::from_secs(5)),
                },
            ),
            (
                400,
                ApnsReason::Unrecognized,
                None,
                ApnsOutcome::Rejected {
                    status: 400,
                    reason: ApnsReason::Unrecognized,
                },
            ),
        ];
        for (status, reason, retry_after, expected) in cases {
            assert_eq!(
                classify_response(status, reason, retry_after, id),
                expected,
                "HTTP {status} {reason:?}"
            );
        }
    }

    #[test]
    fn unrecognized_reason_never_marks_a_device_gone_on_400() {
        assert!(matches!(
            classify_response(400, ApnsReason::Unrecognized, None, None),
            ApnsOutcome::Rejected { .. }
        ));
    }
}
