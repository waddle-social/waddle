//! Typed FCM delivery outcomes and the classification of FCM HTTP v1
//! error responses.
//!
//! FCM reports failures as a `google.rpc.Status` JSON body:
//!
//! ```json
//! {"error": {"code": 404, "status": "NOT_FOUND", "message": "...",
//!   "details": [{"@type": "type.googleapis.com/google.firebase.fcm.v1.FcmError",
//!                "errorCode": "UNREGISTERED"}]}}
//! ```
//!
//! The HTTP status alone is ambiguous (a `404` is both "token
//! unregistered" and "project id does not exist"), so classification
//! reads the body's `error.status`, the `FcmError.errorCode` detail and
//! any `google.rpc.BadRequest.fieldViolations`. Only a response that
//! names the token itself as dead ever disables a device; Google's
//! free-text `message` is never parsed or stored.

use std::fmt;
use std::time::Duration;

use serde::Deserialize;

use crate::push::types::TransientFailure;

/// `@type` suffix of the FCM-specific error detail.
const FCM_ERROR_DETAIL_TYPE_SUFFIX: &str = "google.firebase.fcm.v1.FcmError";
/// `@type` suffix of the request field-violation detail.
const BAD_REQUEST_DETAIL_TYPE_SUFFIX: &str = "google.rpc.BadRequest";
/// The field-violation path that blames the registration token.
const TOKEN_FIELD_PATH: &str = "message.token";

/// Canonical `google.rpc.Code` names FCM returns in `error.status`.
/// Names Google adds later deserialize to [`GoogleRpcStatus::Unrecognized`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum GoogleRpcStatus {
    InvalidArgument,
    Unauthenticated,
    PermissionDenied,
    NotFound,
    ResourceExhausted,
    Unavailable,
    Internal,
    #[serde(other)]
    Unrecognized,
}

impl GoogleRpcStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::Unauthenticated => "UNAUTHENTICATED",
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::NotFound => "NOT_FOUND",
            Self::ResourceExhausted => "RESOURCE_EXHAUSTED",
            Self::Unavailable => "UNAVAILABLE",
            Self::Internal => "INTERNAL",
            Self::Unrecognized => "unrecognized",
        }
    }
}

/// `google.firebase.fcm.v1.FcmError.errorCode` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FcmErrorCode {
    UnspecifiedError,
    InvalidArgument,
    Unregistered,
    SenderIdMismatch,
    QuotaExceeded,
    Unavailable,
    Internal,
    ThirdPartyAuthError,
    #[serde(other)]
    Unrecognized,
}

impl FcmErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnspecifiedError => "UNSPECIFIED_ERROR",
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::Unregistered => "UNREGISTERED",
            Self::SenderIdMismatch => "SENDER_ID_MISMATCH",
            Self::QuotaExceeded => "QUOTA_EXCEEDED",
            Self::Unavailable => "UNAVAILABLE",
            Self::Internal => "INTERNAL",
            Self::ThirdPartyAuthError => "THIRD_PARTY_AUTH_ERROR",
            Self::Unrecognized => "unrecognized",
        }
    }
}

/// The typed parts of an FCM error body. Everything else (notably the
/// free-text `message`) is discarded at parse time.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FcmErrorDetail {
    pub status: Option<GoogleRpcStatus>,
    pub error_code: Option<FcmErrorCode>,
    /// A `google.rpc.BadRequest` field violation names `message.token`.
    pub token_field_violation: bool,
}

#[derive(Deserialize)]
struct ErrorEnvelope {
    error: ErrorBody,
}

#[derive(Deserialize)]
struct ErrorBody {
    #[serde(default)]
    status: Option<GoogleRpcStatus>,
    #[serde(default)]
    details: Vec<ErrorDetailEntry>,
}

#[derive(Deserialize)]
struct ErrorDetailEntry {
    #[serde(rename = "@type", default)]
    type_url: String,
    #[serde(rename = "errorCode", default)]
    error_code: Option<FcmErrorCode>,
    #[serde(rename = "fieldViolations", default)]
    field_violations: Vec<FieldViolation>,
}

#[derive(Deserialize)]
struct FieldViolation {
    #[serde(default)]
    field: String,
}

impl FcmErrorDetail {
    /// Parse an FCM error response body. A body that is not the
    /// `google.rpc.Status` JSON shape yields an empty detail, which never
    /// disables a device.
    pub fn from_body(body: &[u8]) -> Self {
        let Ok(envelope) = serde_json::from_slice::<ErrorEnvelope>(body) else {
            return Self::default();
        };
        let mut detail = Self {
            status: envelope.error.status,
            ..Self::default()
        };
        for entry in envelope.error.details {
            if entry.type_url.ends_with(FCM_ERROR_DETAIL_TYPE_SUFFIX) {
                if let Some(code) = entry.error_code {
                    detail.error_code.get_or_insert(code);
                }
            } else if entry.type_url.ends_with(BAD_REQUEST_DETAIL_TYPE_SUFFIX)
                && entry
                    .field_violations
                    .iter()
                    .any(|violation| violation.field == TOKEN_FIELD_PATH)
            {
                detail.token_field_violation = true;
            }
        }
        detail
    }

    fn is_invalid_argument(&self) -> bool {
        self.error_code == Some(FcmErrorCode::InvalidArgument)
            || self.status == Some(GoogleRpcStatus::InvalidArgument)
    }
}

/// Operator diagnostic built only from typed fields, e.g.
/// `NOT_FOUND/UNREGISTERED`.
impl fmt::Display for FcmErrorDetail {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let status = self.status.map_or("-", GoogleRpcStatus::as_str);
        let code = self.error_code.map_or("-", FcmErrorCode::as_str);
        write!(f, "{status}/{code}")?;
        if self.token_field_violation {
            f.write_str(" field=message.token")?;
        }
        Ok(())
    }
}

/// Why a send is worth retrying later.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FcmTransient {
    /// `429` / `QUOTA_EXCEEDED` / `RESOURCE_EXHAUSTED`: FCM did not
    /// accept the message.
    RateLimited,
    /// Network error, timeout, or an FCM 5xx / `UNAVAILABLE` /
    /// `INTERNAL`: FCM may or may not have accepted the message.
    Failure(TransientFailure),
}

/// Typed result of one FCM send. Every failure mode is a variant; the
/// sender never returns `Result`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FcmOutcome {
    /// `200`: FCM accepted the message.
    Sent,
    /// `UNREGISTERED`: the app was uninstalled or the token expired.
    /// Disables the device.
    Unregistered { status: u16, detail: FcmErrorDetail },
    /// `INVALID_ARGUMENT` blaming `message.token`: the token is not a
    /// valid registration token. Disables the device.
    InvalidToken { status: u16, detail: FcmErrorDetail },
    /// `401 UNAUTHENTICATED`: the OAuth access token was rejected. The
    /// caller invalidates it and retries once with a fresh one.
    Unauthenticated { detail: FcmErrorDetail },
    /// The server's credentials or project are wrong
    /// (`THIRD_PARTY_AUTH_ERROR`, `PERMISSION_DENIED`, a generic `403`,
    /// or a `404` without an FCM error code, i.e. an unknown project).
    /// Permanent and never disables a device.
    ProviderAuth { status: u16, detail: FcmErrorDetail },
    /// `SENDER_ID_MISMATCH`: the token belongs to another Firebase
    /// project. Permanent; the device is kept because the server's
    /// project, not the token, is the usual culprit.
    SenderIdMismatch { status: u16, detail: FcmErrorDetail },
    /// Worth retrying.
    Transient {
        cause: FcmTransient,
        retry_after: Option<Duration>,
    },
    /// Any other rejection (`status: 0` when the request could not be
    /// built locally). Permanent for this message; the device is kept.
    Rejected { status: u16, detail: FcmErrorDetail },
}

/// Map an FCM HTTP response to its typed outcome. Pure so the whole
/// table is unit-testable without a server.
pub fn classify_response(
    status: u16,
    detail: FcmErrorDetail,
    retry_after: Option<Duration>,
) -> FcmOutcome {
    if (200..300).contains(&status) {
        return FcmOutcome::Sent;
    }
    let rate_limited = FcmOutcome::Transient {
        cause: FcmTransient::RateLimited,
        retry_after,
    };
    let server_error = FcmOutcome::Transient {
        cause: FcmTransient::Failure(TransientFailure::ServerError { status }),
        retry_after,
    };
    // The FCM-specific error code is the most precise signal.
    match detail.error_code {
        Some(FcmErrorCode::Unregistered) => return FcmOutcome::Unregistered { status, detail },
        Some(FcmErrorCode::SenderIdMismatch) => {
            return FcmOutcome::SenderIdMismatch { status, detail }
        }
        Some(FcmErrorCode::QuotaExceeded) => return rate_limited,
        Some(FcmErrorCode::Unavailable | FcmErrorCode::Internal) => return server_error,
        Some(FcmErrorCode::ThirdPartyAuthError) => {
            return FcmOutcome::ProviderAuth { status, detail }
        }
        Some(
            FcmErrorCode::InvalidArgument
            | FcmErrorCode::UnspecifiedError
            | FcmErrorCode::Unrecognized,
        )
        | None => {}
    }
    if detail.is_invalid_argument() {
        return if detail.token_field_violation {
            FcmOutcome::InvalidToken { status, detail }
        } else {
            FcmOutcome::Rejected { status, detail }
        };
    }
    match (status, detail.status) {
        (401, _) | (_, Some(GoogleRpcStatus::Unauthenticated)) => {
            FcmOutcome::Unauthenticated { detail }
        }
        (429, _) | (_, Some(GoogleRpcStatus::ResourceExhausted)) => rate_limited,
        (500..=599, _) | (_, Some(GoogleRpcStatus::Unavailable | GoogleRpcStatus::Internal)) => {
            server_error
        }
        // Without an FCM error code a 404 is an unknown project (or a
        // wrong URL), never a dead token: devices must survive it.
        (403 | 404, _)
        | (_, Some(GoogleRpcStatus::PermissionDenied | GoogleRpcStatus::NotFound)) => {
            FcmOutcome::ProviderAuth { status, detail }
        }
        _ => FcmOutcome::Rejected { status, detail },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FCM_ERROR: &str = "type.googleapis.com/google.firebase.fcm.v1.FcmError";
    const BAD_REQUEST: &str = "type.googleapis.com/google.rpc.BadRequest";

    fn body(code: u16, status: &str, details: serde_json::Value) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "error": {
                "code": code,
                "message": "free text that must never be stored",
                "status": status,
                "details": details,
            }
        }))
        .expect("json")
    }

    fn fcm_error(code: &str) -> serde_json::Value {
        serde_json::json!([{ "@type": FCM_ERROR, "errorCode": code }])
    }

    fn classify(status: u16, body: &[u8]) -> FcmOutcome {
        classify_response(status, FcmErrorDetail::from_body(body), None)
    }

    #[test]
    fn body_parsing_extracts_status_error_code_and_token_violation() {
        let detail = FcmErrorDetail::from_body(&body(
            400,
            "INVALID_ARGUMENT",
            serde_json::json!([
                { "@type": FCM_ERROR, "errorCode": "INVALID_ARGUMENT" },
                { "@type": BAD_REQUEST, "fieldViolations": [
                    { "field": "message.token", "description": "Invalid registration token" }
                ]},
            ]),
        ));
        assert_eq!(detail.status, Some(GoogleRpcStatus::InvalidArgument));
        assert_eq!(detail.error_code, Some(FcmErrorCode::InvalidArgument));
        assert!(detail.token_field_violation);
        assert_eq!(
            detail.to_string(),
            "INVALID_ARGUMENT/INVALID_ARGUMENT field=message.token"
        );
    }

    #[test]
    fn unknown_or_garbage_bodies_parse_to_an_empty_detail() {
        assert_eq!(FcmErrorDetail::from_body(b""), FcmErrorDetail::default());
        assert_eq!(
            FcmErrorDetail::from_body(b"<html>"),
            FcmErrorDetail::default()
        );
        let detail = FcmErrorDetail::from_body(&body(400, "SOMETHING_NEW", fcm_error("NEW_CODE")));
        assert_eq!(detail.status, Some(GoogleRpcStatus::Unrecognized));
        assert_eq!(detail.error_code, Some(FcmErrorCode::Unrecognized));
    }

    #[test]
    fn error_code_from_a_foreign_detail_type_is_ignored() {
        let detail = FcmErrorDetail::from_body(&body(
            404,
            "NOT_FOUND",
            serde_json::json!([{ "@type": "type.example.com/Other", "errorCode": "UNREGISTERED" }]),
        ));
        assert_eq!(detail.error_code, None);
        assert!(matches!(
            classify_response(404, detail, None),
            FcmOutcome::ProviderAuth { .. }
        ));
    }

    #[test]
    fn diagnostic_never_contains_google_free_text() {
        let detail = FcmErrorDetail::from_body(&body(404, "NOT_FOUND", fcm_error("UNREGISTERED")));
        assert_eq!(detail.to_string(), "NOT_FOUND/UNREGISTERED");
        assert!(!detail.to_string().contains("free text"));
    }

    #[test]
    fn classification_table() {
        let token_violation = serde_json::json!([
            { "@type": FCM_ERROR, "errorCode": "INVALID_ARGUMENT" },
            { "@type": BAD_REQUEST, "fieldViolations": [{ "field": "message.token" }] },
        ]);
        let other_violation = serde_json::json!([
            { "@type": FCM_ERROR, "errorCode": "INVALID_ARGUMENT" },
            { "@type": BAD_REQUEST, "fieldViolations": [{ "field": "message.android.ttl" }] },
        ]);
        let cases: Vec<(u16, Vec<u8>, &str)> = vec![
            (200, Vec::new(), "sent"),
            (
                404,
                body(404, "NOT_FOUND", fcm_error("UNREGISTERED")),
                "unregistered",
            ),
            (
                400,
                body(400, "INVALID_ARGUMENT", token_violation),
                "invalid-token",
            ),
            (
                400,
                body(400, "INVALID_ARGUMENT", other_violation),
                "rejected",
            ),
            (
                400,
                body(400, "INVALID_ARGUMENT", fcm_error("INVALID_ARGUMENT")),
                "rejected",
            ),
            (
                403,
                body(403, "PERMISSION_DENIED", fcm_error("SENDER_ID_MISMATCH")),
                "sender-id-mismatch",
            ),
            (
                429,
                body(429, "RESOURCE_EXHAUSTED", fcm_error("QUOTA_EXCEEDED")),
                "rate-limited",
            ),
            (429, Vec::new(), "rate-limited"),
            (
                503,
                body(503, "UNAVAILABLE", fcm_error("UNAVAILABLE")),
                "server-error",
            ),
            (
                500,
                body(500, "INTERNAL", fcm_error("INTERNAL")),
                "server-error",
            ),
            (502, b"<html>bad gateway</html>".to_vec(), "server-error"),
            (
                401,
                body(401, "UNAUTHENTICATED", serde_json::json!([])),
                "unauthenticated",
            ),
            (401, Vec::new(), "unauthenticated"),
            (
                401,
                body(401, "UNAUTHENTICATED", fcm_error("THIRD_PARTY_AUTH_ERROR")),
                "provider-auth",
            ),
            (
                403,
                body(403, "PERMISSION_DENIED", serde_json::json!([])),
                "provider-auth",
            ),
            (403, Vec::new(), "provider-auth"),
            // A bare 404 is a wrong project id, never a dead token.
            (404, Vec::new(), "provider-auth"),
            (
                404,
                body(404, "NOT_FOUND", serde_json::json!([])),
                "provider-auth",
            ),
            (
                400,
                body(400, "FAILED_PRECONDITION", serde_json::json!([])),
                "rejected",
            ),
            (418, Vec::new(), "rejected"),
        ];
        for (status, raw, expected) in cases {
            let outcome = classify(status, &raw);
            let actual = match outcome {
                FcmOutcome::Sent => "sent",
                FcmOutcome::Unregistered { .. } => "unregistered",
                FcmOutcome::InvalidToken { .. } => "invalid-token",
                FcmOutcome::Unauthenticated { .. } => "unauthenticated",
                FcmOutcome::ProviderAuth { .. } => "provider-auth",
                FcmOutcome::SenderIdMismatch { .. } => "sender-id-mismatch",
                FcmOutcome::Transient {
                    cause: FcmTransient::RateLimited,
                    ..
                } => "rate-limited",
                FcmOutcome::Transient {
                    cause: FcmTransient::Failure(TransientFailure::ServerError { .. }),
                    ..
                } => "server-error",
                FcmOutcome::Transient { .. } => "network",
                FcmOutcome::Rejected { .. } => "rejected",
            };
            assert_eq!(
                actual,
                expected,
                "HTTP {status} {}",
                String::from_utf8_lossy(&raw)
            );
        }
    }

    #[test]
    fn retry_after_is_carried_on_rate_limits() {
        assert_eq!(
            classify_response(
                429,
                FcmErrorDetail::default(),
                Some(Duration::from_secs(30))
            ),
            FcmOutcome::Transient {
                cause: FcmTransient::RateLimited,
                retry_after: Some(Duration::from_secs(30)),
            }
        );
    }
}
