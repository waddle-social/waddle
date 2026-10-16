//! Validated FCM identifiers.
//!
//! Every value that ends up in an FCM request URL or body is parsed into
//! one of these newtypes exactly once, so the sender never has to
//! re-validate (or escape) a raw string.

use thiserror::Error;

/// Upper bound on a Google Cloud project id. Current ids are 6..=30
/// characters; legacy domain-scoped ids (`example.com:project`) are
/// longer, so the bound is generous rather than exact.
const MAX_PROJECT_ID_LEN: usize = 100;
/// Upper bound on a registration token. FCM tokens are a few hundred
/// characters today; Google does not document a maximum, so this matches
/// the Push Service's `provider_token` storage bound.
const MAX_REGISTRATION_TOKEN_LEN: usize = 4096;
/// Upper bound on an Android application id (package name).
const MAX_APP_ID_LEN: usize = 255;

/// Why an FCM identifier failed validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum FcmIdentifierError {
    #[error("FCM project id must be 1..={MAX_PROJECT_ID_LEN} characters of [a-z0-9-.:]")]
    ProjectId,
    #[error(
        "FCM registration token must be 1..={MAX_REGISTRATION_TOKEN_LEN} characters of [A-Za-z0-9-_:.]"
    )]
    RegistrationToken,
    #[error("FCM application id must be 1..={MAX_APP_ID_LEN} characters of [A-Za-z0-9._]")]
    AppId,
}

/// Google Cloud project id: the `{project}` segment of
/// `POST /v1/projects/{project}/messages:send`. The charset excludes
/// `/`, `?` and `#`, which is what keeps the request path well-formed.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FcmProjectId(String);

impl FcmProjectId {
    pub fn parse(value: &str) -> Result<Self, FcmIdentifierError> {
        let valid = !value.is_empty()
            && value.len() <= MAX_PROJECT_ID_LEN
            && value.bytes().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'-' | b'.' | b':')
            });
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(FcmIdentifierError::ProjectId)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// FCM registration token minted by the Firebase SDK on the device. It
/// is carried in the JSON body (`message.token`), never in a URL.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct FcmRegistrationToken(String);

impl FcmRegistrationToken {
    pub fn parse(value: &str) -> Result<Self, FcmIdentifierError> {
        let valid = !value.is_empty()
            && value.len() <= MAX_REGISTRATION_TOKEN_LEN
            && value.bytes().all(|byte| {
                byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b':' | b'.')
            });
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(FcmIdentifierError::RegistrationToken)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A registration token addresses one device: never print it.
impl std::fmt::Debug for FcmRegistrationToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("FcmRegistrationToken(<redacted>)")
    }
}

/// Android application id (package name) the Push Service delivers for,
/// e.g. `social.waddle.android`. Registrations for any other app id are
/// refused before anything is sent under this server's credentials.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct FcmAppId(String);

impl FcmAppId {
    pub fn parse(value: &str) -> Result<Self, FcmIdentifierError> {
        let valid = !value.is_empty()
            && value.len() <= MAX_APP_ID_LEN
            && value
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_'));
        if valid {
            Ok(Self(value.to_owned()))
        } else {
            Err(FcmIdentifierError::AppId)
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn project_id_accepts_google_project_ids() {
        assert!(FcmProjectId::parse("waddle-social").is_ok());
        assert!(FcmProjectId::parse("example.com:legacy-project").is_ok());
        for bad in [
            "",
            "Upper",
            "a/b",
            "a?b",
            "a#b",
            "has space",
            &"a".repeat(101),
        ] {
            assert_eq!(
                FcmProjectId::parse(bad),
                Err(FcmIdentifierError::ProjectId),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn registration_token_accepts_the_firebase_alphabet() {
        let token = "cXyZ-12_ab:APA91bHun4MxP5egoKMwt2KZFBaFUH-1RYqx";
        assert_eq!(
            FcmRegistrationToken::parse(token)
                .expect("firebase token")
                .as_str(),
            token
        );
        for bad in ["", "has space", "quote\"", "new\nline", &"a".repeat(4097)] {
            assert_eq!(
                FcmRegistrationToken::parse(bad),
                Err(FcmIdentifierError::RegistrationToken),
                "{bad:?} must be rejected"
            );
        }
    }

    #[test]
    fn registration_token_debug_is_redacted() {
        let token = FcmRegistrationToken::parse("secret-token").expect("token");
        assert!(!format!("{token:?}").contains("secret-token"));
    }

    #[test]
    fn app_id_accepts_android_package_names() {
        assert!(FcmAppId::parse("social.waddle.android").is_ok());
        assert!(FcmAppId::parse("com.example.my_app").is_ok());
        for bad in ["", "bad app", "a/b", "a-b", &"a".repeat(256)] {
            assert_eq!(FcmAppId::parse(bad), Err(FcmIdentifierError::AppId));
        }
    }
}
