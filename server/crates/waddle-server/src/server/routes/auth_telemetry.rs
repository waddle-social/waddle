use crate::auth::AuthError;
use jid::BareJid;
use tracing::warn;
use waddle_xmpp::telemetry::attributes::{AuthErrorCode, AuthStage, MetricAttribute};

#[derive(Debug, Clone, Copy)]
pub(super) enum AuthFailure<'a> {
    Error {
        stage: AuthStage,
        error: &'a AuthError,
    },
    AuthorizationRejected,
    AuthorizationInvalidClient,
    AuthorizationMalformed,
    CallbackInvalidCode,
    CallbackInvalidClient,
    TokenInvalidCode,
    TokenInvalidGrant,
    TokenExpired,
    TokenMalformed,
    StateMismatch,
    StateExpired,
    DeviceInvalidCode,
    DeviceInvalidGrant,
    DeviceInvalidClient,
    DeviceExpired,
    DeviceMalformed,
    DeviceOther,
    ScramMalformed,
    ScramInvalidCredentials,
    ScramUnknownUser,
    ScramOther,
}

impl<'a> AuthFailure<'a> {
    pub(super) fn from_error(stage: AuthStage, error: &'a AuthError) -> Self {
        Self::Error { stage, error }
    }
}

pub(super) fn classify_auth_failure(failure: AuthFailure<'_>) -> (AuthStage, AuthErrorCode) {
    match failure {
        AuthFailure::Error { stage, error } => {
            let error_code = match error {
                AuthError::InvalidProvider(_) => AuthErrorCode::InvalidClient,
                AuthError::InvalidRequest(_) => AuthErrorCode::Malformed,
                AuthError::InvalidState | AuthError::InvalidNonce => AuthErrorCode::InvalidState,
                AuthError::AuthorizationFailed(_) => AuthErrorCode::Other,
                AuthError::TokenExchangeFailed(_) => AuthErrorCode::InvalidGrant,
                AuthError::UserInfoFailed(_) => AuthErrorCode::UserinfoFailed,
                AuthError::JwtError(_) | AuthError::InvalidPassword(_) => {
                    AuthErrorCode::InvalidCredentials
                }
                AuthError::SessionNotFound(_) => AuthErrorCode::InvalidGrant,
                AuthError::SessionExpired => AuthErrorCode::Expired,
                AuthError::DatabaseError(_)
                | AuthError::UserAlreadyExists(_)
                | AuthError::CryptoError(_)
                | AuthError::RegistrationDisabled => AuthErrorCode::Other,
                AuthError::HttpError(_) => AuthErrorCode::ProviderUnreachable,
                AuthError::UserNotFound(_) => AuthErrorCode::UnknownUser,
                AuthError::InvalidUsername(_) => AuthErrorCode::Malformed,
            };
            (stage, error_code)
        }
        AuthFailure::AuthorizationRejected => (AuthStage::OidcAuthorization, AuthErrorCode::Other),
        AuthFailure::AuthorizationInvalidClient => {
            (AuthStage::OidcAuthorization, AuthErrorCode::InvalidClient)
        }
        AuthFailure::AuthorizationMalformed => {
            (AuthStage::OidcAuthorization, AuthErrorCode::Malformed)
        }
        AuthFailure::CallbackInvalidCode => (AuthStage::OidcCallback, AuthErrorCode::InvalidCode),
        AuthFailure::CallbackInvalidClient => {
            (AuthStage::OidcCallback, AuthErrorCode::InvalidClient)
        }
        AuthFailure::TokenInvalidCode => (AuthStage::TokenExchange, AuthErrorCode::InvalidCode),
        AuthFailure::TokenInvalidGrant => (AuthStage::TokenExchange, AuthErrorCode::InvalidGrant),
        AuthFailure::TokenExpired => (AuthStage::TokenExchange, AuthErrorCode::Expired),
        AuthFailure::TokenMalformed => (AuthStage::TokenExchange, AuthErrorCode::Malformed),
        AuthFailure::StateMismatch => (AuthStage::State, AuthErrorCode::InvalidState),
        AuthFailure::StateExpired => (AuthStage::State, AuthErrorCode::Expired),
        AuthFailure::DeviceInvalidCode => (AuthStage::DeviceFlow, AuthErrorCode::InvalidCode),
        AuthFailure::DeviceInvalidGrant => (AuthStage::DeviceFlow, AuthErrorCode::InvalidGrant),
        AuthFailure::DeviceInvalidClient => (AuthStage::DeviceFlow, AuthErrorCode::InvalidClient),
        AuthFailure::DeviceExpired => (AuthStage::DeviceFlow, AuthErrorCode::Expired),
        AuthFailure::DeviceMalformed => (AuthStage::DeviceFlow, AuthErrorCode::Malformed),
        AuthFailure::DeviceOther => (AuthStage::DeviceFlow, AuthErrorCode::Other),
        AuthFailure::ScramMalformed => (AuthStage::Scram, AuthErrorCode::Malformed),
        AuthFailure::ScramInvalidCredentials => {
            (AuthStage::Scram, AuthErrorCode::InvalidCredentials)
        }
        AuthFailure::ScramUnknownUser => (AuthStage::Scram, AuthErrorCode::UnknownUser),
        AuthFailure::ScramOther => (AuthStage::Scram, AuthErrorCode::Other),
    }
}

/// Record an auth rejection at the shared metric/log choke point.
///
/// The failure-ratio alert rule follows in the #1324 alerts tree once
/// production baselines have been observed.
pub(super) fn record_auth_failure(
    provider: &str,
    bare_identifier: Option<&BareJid>,
    failure: AuthFailure<'_>,
) {
    let (stage, error_code) = classify_auth_failure(failure);

    match bare_identifier {
        Some(identifier) => warn!(
            provider,
            stage = stage.value(),
            error_code = error_code.value(),
            bare_identifier = %identifier,
            "Authentication rejected",
        ),
        None => warn!(
            provider,
            stage = stage.value(),
            error_code = error_code.value(),
            "Authentication rejected",
        ),
    }

    waddle_xmpp::counter_add!(
        "waddle.auth.failures",
        "1",
        "Authentication rejections by stage and enumerated error code.",
        1,
        stage,
        error_code,
    );
}

pub(super) fn record_auth_success(stage: AuthStage) {
    waddle_xmpp::counter_add!(
        "waddle.auth.success",
        "1",
        "Successful authentication outcomes by stage.",
        1,
        stage,
    );
}

#[cfg(test)]
mod tests {
    use super::{classify_auth_failure, record_auth_failure, record_auth_success, AuthFailure};
    use crate::auth::AuthError;
    use waddle_xmpp::telemetry::attributes::{AuthErrorCode, AuthStage};
    use waddle_xmpp::telemetry::test_support;

    #[tokio::test]
    async fn concrete_auth_errors_map_and_export_exhaustively_without_inspecting_messages() {
        let guard = test_support::acquire().await;
        let cases = [
            (
                AuthError::InvalidProvider("provider".to_string()),
                AuthErrorCode::InvalidClient,
            ),
            (
                AuthError::InvalidRequest("request".to_string()),
                AuthErrorCode::Malformed,
            ),
            (AuthError::InvalidState, AuthErrorCode::InvalidState),
            (AuthError::InvalidNonce, AuthErrorCode::InvalidState),
            (
                AuthError::AuthorizationFailed("authorization".to_string()),
                AuthErrorCode::Other,
            ),
            (
                AuthError::TokenExchangeFailed("exchange".to_string()),
                AuthErrorCode::InvalidGrant,
            ),
            (
                AuthError::UserInfoFailed("userinfo".to_string()),
                AuthErrorCode::UserinfoFailed,
            ),
            (
                AuthError::JwtError("jwt".to_string()),
                AuthErrorCode::InvalidCredentials,
            ),
            (
                AuthError::SessionNotFound("session".to_string()),
                AuthErrorCode::InvalidGrant,
            ),
            (AuthError::SessionExpired, AuthErrorCode::Expired),
            (
                AuthError::DatabaseError("database".to_string()),
                AuthErrorCode::Other,
            ),
            (
                AuthError::HttpError("transport".to_string()),
                AuthErrorCode::ProviderUnreachable,
            ),
            (
                AuthError::UserAlreadyExists("user".to_string()),
                AuthErrorCode::Other,
            ),
            (
                AuthError::UserNotFound("user".to_string()),
                AuthErrorCode::UnknownUser,
            ),
            (
                AuthError::InvalidUsername("username".to_string()),
                AuthErrorCode::Malformed,
            ),
            (
                AuthError::InvalidPassword("password".to_string()),
                AuthErrorCode::InvalidCredentials,
            ),
            (
                AuthError::CryptoError("crypto".to_string()),
                AuthErrorCode::Other,
            ),
            (AuthError::RegistrationDisabled, AuthErrorCode::Other),
        ];

        for (error, expected_code) in cases {
            assert_eq!(
                classify_auth_failure(AuthFailure::from_error(AuthStage::OidcCallback, &error,)),
                (AuthStage::OidcCallback, expected_code),
            );
            record_auth_failure(
                "unknown",
                None,
                AuthFailure::from_error(AuthStage::OidcCallback, &error),
            );
        }

        let expected_counts = [
            ("invalid_client", 1),
            ("malformed", 2),
            ("invalid_state", 2),
            ("other", 5),
            ("invalid_grant", 2),
            ("userinfo_failed", 1),
            ("invalid_credentials", 2),
            ("expired", 1),
            ("provider_unreachable", 1),
            ("unknown_user", 1),
        ];
        for (error_code, expected_count) in expected_counts {
            assert_eq!(
                guard.counter_sum(
                    "waddle.auth.failures",
                    &[("stage", "oidc_callback"), ("error_code", error_code),],
                ),
                Some(expected_count),
                "wrong exported count for {error_code}",
            );
        }
    }

    #[tokio::test]
    async fn auth_counters_export_every_stage_and_error_code_at_the_reader_seam() {
        let guard = test_support::acquire().await;
        let provider_unreachable = AuthError::HttpError("transport".to_string());
        let unknown_user = AuthError::UserNotFound("user".to_string());
        let userinfo_rejected = AuthError::UserInfoFailed("userinfo".to_string());
        let failure_cases = [
            (
                AuthFailure::AuthorizationRejected,
                AuthStage::OidcAuthorization,
                AuthErrorCode::Other,
                "oidc_authorization",
                "other",
            ),
            (
                AuthFailure::AuthorizationInvalidClient,
                AuthStage::OidcAuthorization,
                AuthErrorCode::InvalidClient,
                "oidc_authorization",
                "invalid_client",
            ),
            (
                AuthFailure::AuthorizationMalformed,
                AuthStage::OidcAuthorization,
                AuthErrorCode::Malformed,
                "oidc_authorization",
                "malformed",
            ),
            (
                AuthFailure::CallbackInvalidCode,
                AuthStage::OidcCallback,
                AuthErrorCode::InvalidCode,
                "oidc_callback",
                "invalid_code",
            ),
            (
                AuthFailure::CallbackInvalidClient,
                AuthStage::OidcCallback,
                AuthErrorCode::InvalidClient,
                "oidc_callback",
                "invalid_client",
            ),
            (
                AuthFailure::TokenInvalidCode,
                AuthStage::TokenExchange,
                AuthErrorCode::InvalidCode,
                "token_exchange",
                "invalid_code",
            ),
            (
                AuthFailure::TokenInvalidGrant,
                AuthStage::TokenExchange,
                AuthErrorCode::InvalidGrant,
                "token_exchange",
                "invalid_grant",
            ),
            (
                AuthFailure::TokenExpired,
                AuthStage::TokenExchange,
                AuthErrorCode::Expired,
                "token_exchange",
                "expired",
            ),
            (
                AuthFailure::TokenMalformed,
                AuthStage::TokenExchange,
                AuthErrorCode::Malformed,
                "token_exchange",
                "malformed",
            ),
            (
                AuthFailure::StateMismatch,
                AuthStage::State,
                AuthErrorCode::InvalidState,
                "state",
                "invalid_state",
            ),
            (
                AuthFailure::StateExpired,
                AuthStage::State,
                AuthErrorCode::Expired,
                "state",
                "expired",
            ),
            (
                AuthFailure::DeviceInvalidCode,
                AuthStage::DeviceFlow,
                AuthErrorCode::InvalidCode,
                "device_flow",
                "invalid_code",
            ),
            (
                AuthFailure::DeviceInvalidGrant,
                AuthStage::DeviceFlow,
                AuthErrorCode::InvalidGrant,
                "device_flow",
                "invalid_grant",
            ),
            (
                AuthFailure::DeviceInvalidClient,
                AuthStage::DeviceFlow,
                AuthErrorCode::InvalidClient,
                "device_flow",
                "invalid_client",
            ),
            (
                AuthFailure::DeviceExpired,
                AuthStage::DeviceFlow,
                AuthErrorCode::Expired,
                "device_flow",
                "expired",
            ),
            (
                AuthFailure::DeviceMalformed,
                AuthStage::DeviceFlow,
                AuthErrorCode::Malformed,
                "device_flow",
                "malformed",
            ),
            (
                AuthFailure::DeviceOther,
                AuthStage::DeviceFlow,
                AuthErrorCode::Other,
                "device_flow",
                "other",
            ),
            (
                AuthFailure::ScramMalformed,
                AuthStage::Scram,
                AuthErrorCode::Malformed,
                "scram",
                "malformed",
            ),
            (
                AuthFailure::ScramInvalidCredentials,
                AuthStage::Scram,
                AuthErrorCode::InvalidCredentials,
                "scram",
                "invalid_credentials",
            ),
            (
                AuthFailure::ScramUnknownUser,
                AuthStage::Scram,
                AuthErrorCode::UnknownUser,
                "scram",
                "unknown_user",
            ),
            (
                AuthFailure::ScramOther,
                AuthStage::Scram,
                AuthErrorCode::Other,
                "scram",
                "other",
            ),
            (
                AuthFailure::from_error(AuthStage::Userinfo, &userinfo_rejected),
                AuthStage::Userinfo,
                AuthErrorCode::UserinfoFailed,
                "userinfo",
                "userinfo_failed",
            ),
            (
                AuthFailure::from_error(AuthStage::OidcAuthorization, &provider_unreachable),
                AuthStage::OidcAuthorization,
                AuthErrorCode::ProviderUnreachable,
                "oidc_authorization",
                "provider_unreachable",
            ),
            (
                AuthFailure::from_error(AuthStage::OidcCallback, &unknown_user),
                AuthStage::OidcCallback,
                AuthErrorCode::UnknownUser,
                "oidc_callback",
                "unknown_user",
            ),
        ];

        for &(failure, stage, error_code, _, _) in &failure_cases {
            assert_eq!(classify_auth_failure(failure), (stage, error_code));
            record_auth_failure("unknown", None, failure);
        }

        let stages = [
            (AuthStage::OidcAuthorization, "oidc_authorization"),
            (AuthStage::OidcCallback, "oidc_callback"),
            (AuthStage::TokenExchange, "token_exchange"),
            (AuthStage::Userinfo, "userinfo"),
            (AuthStage::State, "state"),
            (AuthStage::DeviceFlow, "device_flow"),
            (AuthStage::Scram, "scram"),
        ];
        for (stage, _) in stages {
            record_auth_success(stage);
        }

        for (stage, stage_value) in stages {
            assert_eq!(
                guard.counter_sum("waddle.auth.success", &[("stage", stage_value)],),
                Some(1),
                "missing success sample for {stage:?}",
            );
        }

        for &(_, _, _, stage, error_code) in &failure_cases {
            assert_eq!(
                guard.counter_sum(
                    "waddle.auth.failures",
                    &[("stage", stage), ("error_code", error_code)],
                ),
                Some(1),
                "missing failure sample for {stage}/{error_code}",
            );
        }

        assert_eq!(
            guard.metric_unit("waddle.auth.failures"),
            Some("1".to_string())
        );
        assert_eq!(
            guard.metric_unit("waddle.auth.success"),
            Some("1".to_string())
        );
    }
}
