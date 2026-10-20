//! Operator configuration for FCM delivery (#530).
//!
//! FCM is configured with three variables that only make sense
//! together:
//!
//! - `WADDLE_FCM_PROJECT_ID`: the Firebase / Google Cloud project id
//!   (`{project}` in `projects/{project}/messages:send`).
//! - `WADDLE_FCM_SERVICE_ACCOUNT_FILE`: path to the service-account JSON
//!   key the server signs OAuth assertions with.
//! - `WADDLE_FCM_APP_ID`: the Android application id (package name)
//!   this server delivers for, e.g. `social.waddle.android`. Nodes
//!   registered for any other app id are refused before anything is
//!   unsealed or sent.
//!
//! Setting none leaves FCM disabled (Android devices record
//! `fcm-not-configured`). Setting some but not all is a startup error,
//! as is a key file that cannot be read or used, so a half-configured
//! deployment never boots silently undeliverable.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use waddle_xmpp::push::fcm::{
    FcmAppId, FcmIdentifierError, FcmProjectId, FcmServiceAccountError, FcmServiceAccountKey,
    FcmTokenSourceBuildError, OAuthFcmTokenSource, SystemFcmClock,
};

/// One of the `WADDLE_FCM_*` variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FcmConfigVar {
    ProjectId,
    ServiceAccountFile,
    AppId,
}

impl FcmConfigVar {
    const ALL: [Self; 3] = [Self::ProjectId, Self::ServiceAccountFile, Self::AppId];

    pub fn name(self) -> &'static str {
        match self {
            Self::ProjectId => "WADDLE_FCM_PROJECT_ID",
            Self::ServiceAccountFile => "WADDLE_FCM_SERVICE_ACCOUNT_FILE",
            Self::AppId => "WADDLE_FCM_APP_ID",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|var| var.name() == name)
    }
}

impl fmt::Display for FcmConfigVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

fn var_list(vars: &[FcmConfigVar]) -> String {
    vars.iter()
        .map(|var| var.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Why the `WADDLE_FCM_*` configuration was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FcmConfigError {
    #[error("FCM configuration is incomplete: set all of WADDLE_FCM_PROJECT_ID, WADDLE_FCM_SERVICE_ACCOUNT_FILE and WADDLE_FCM_APP_ID, or none; missing {}", var_list(.missing))]
    Incomplete { missing: Vec<FcmConfigVar> },
    #[error("{var} is invalid: {source}")]
    Invalid {
        var: FcmConfigVar,
        #[source]
        source: FcmIdentifierError,
    },
}

/// Why the configured service-account key could not be turned into an
/// access-token source.
#[derive(Debug, thiserror::Error)]
pub enum FcmTokenSourceLoadError {
    #[error("failed to read WADDLE_FCM_SERVICE_ACCOUNT_FILE {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error(
        "WADDLE_FCM_SERVICE_ACCOUNT_FILE {path} is not a usable service-account key: {source}"
    )]
    Key {
        path: PathBuf,
        #[source]
        source: FcmServiceAccountError,
    },
    #[error("failed to build the FCM OAuth client: {0}")]
    Client(#[source] FcmTokenSourceBuildError),
}

/// Validated FCM configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FcmConfig {
    pub project_id: FcmProjectId,
    pub service_account_file: PathBuf,
    pub app_id: FcmAppId,
}

#[derive(Default)]
struct RawFcmVars {
    project_id: Option<String>,
    service_account_file: Option<String>,
    app_id: Option<String>,
}

impl RawFcmVars {
    fn slot(&mut self, var: FcmConfigVar) -> &mut Option<String> {
        match var {
            FcmConfigVar::ProjectId => &mut self.project_id,
            FcmConfigVar::ServiceAccountFile => &mut self.service_account_file,
            FcmConfigVar::AppId => &mut self.app_id,
        }
    }
}

impl FcmConfig {
    /// `Ok(None)` when no `WADDLE_FCM_*` variable is set.
    pub fn from_env() -> Result<Option<Self>, FcmConfigError> {
        Self::from_vars(std::env::vars())
    }

    /// Testable form of [`Self::from_env`]. Empty values count as unset.
    pub fn from_vars<I, K, V>(vars: I) -> Result<Option<Self>, FcmConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut raw = RawFcmVars::default();
        for (key, value) in vars {
            let value = value.as_ref().trim();
            if value.is_empty() {
                continue;
            }
            if let Some(var) = FcmConfigVar::from_name(key.as_ref()) {
                *raw.slot(var) = Some(value.to_owned());
            }
        }
        let RawFcmVars {
            project_id: Some(project_id),
            service_account_file: Some(service_account_file),
            app_id: Some(app_id),
        } = raw
        else {
            let missing = FcmConfigVar::ALL
                .into_iter()
                .filter(|var| raw.slot(*var).is_none())
                .collect::<Vec<_>>();
            return if missing.len() == FcmConfigVar::ALL.len() {
                Ok(None)
            } else {
                Err(FcmConfigError::Incomplete { missing })
            };
        };
        let invalid = |var| move |source| FcmConfigError::Invalid { var, source };
        Ok(Some(Self {
            project_id: FcmProjectId::parse(&project_id)
                .map_err(invalid(FcmConfigVar::ProjectId))?,
            service_account_file: PathBuf::from(service_account_file),
            app_id: FcmAppId::parse(&app_id).map_err(invalid(FcmConfigVar::AppId))?,
        }))
    }

    /// Read the service-account key once and build the caching OAuth
    /// token source. Failing here fails boot: a configured-but-broken
    /// FCM key must not degrade silently.
    pub async fn load_token_source(&self) -> Result<OAuthFcmTokenSource, FcmTokenSourceLoadError> {
        let json =
            zeroize::Zeroizing::new(tokio::fs::read(&self.service_account_file).await.map_err(
                |source| FcmTokenSourceLoadError::Read {
                    path: self.service_account_file.clone(),
                    source,
                },
            )?);
        let key = FcmServiceAccountKey::from_json(&json).map_err(|source| {
            FcmTokenSourceLoadError::Key {
                path: self.service_account_file.clone(),
                source,
            }
        })?;
        OAuthFcmTokenSource::new(key, Arc::new(SystemFcmClock))
            .map_err(FcmTokenSourceLoadError::Client)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: [(&str, &str); 3] = [
        ("WADDLE_FCM_PROJECT_ID", "waddle-social"),
        (
            "WADDLE_FCM_SERVICE_ACCOUNT_FILE",
            "/run/secrets/fcm-service-account.json",
        ),
        ("WADDLE_FCM_APP_ID", "social.waddle.android"),
    ];

    #[test]
    fn no_fcm_vars_disables_fcm() {
        assert_eq!(
            FcmConfig::from_vars([("WADDLE_MODE", "homeserver")]),
            Ok(None)
        );
    }

    #[test]
    fn empty_fcm_vars_count_as_unset() {
        let empty = FULL.map(|(key, _)| (key, ""));
        assert_eq!(FcmConfig::from_vars(empty), Ok(None));
    }

    #[test]
    fn all_fcm_vars_produce_a_typed_config() {
        let config = FcmConfig::from_vars(FULL)
            .expect("valid config")
            .expect("configured");
        assert_eq!(config.project_id.as_str(), "waddle-social");
        assert_eq!(
            config.service_account_file,
            PathBuf::from("/run/secrets/fcm-service-account.json")
        );
        assert_eq!(config.app_id.as_str(), "social.waddle.android");
    }

    #[test]
    fn partial_fcm_vars_are_a_startup_error_naming_the_missing_ones() {
        let err = FcmConfig::from_vars(FULL[..1].iter().copied()).expect_err("partial config");
        assert_eq!(
            err,
            FcmConfigError::Incomplete {
                missing: vec![FcmConfigVar::ServiceAccountFile, FcmConfigVar::AppId]
            }
        );
        assert!(err
            .to_string()
            .contains("missing WADDLE_FCM_SERVICE_ACCOUNT_FILE, WADDLE_FCM_APP_ID"));
    }

    #[test]
    fn malformed_identifiers_are_typed_errors() {
        let mut vars = FULL;
        vars[0] = ("WADDLE_FCM_PROJECT_ID", "Not/A/Project");
        assert_eq!(
            FcmConfig::from_vars(vars),
            Err(FcmConfigError::Invalid {
                var: FcmConfigVar::ProjectId,
                source: FcmIdentifierError::ProjectId
            })
        );
        let mut vars = FULL;
        vars[2] = ("WADDLE_FCM_APP_ID", "not an app");
        assert_eq!(
            FcmConfig::from_vars(vars),
            Err(FcmConfigError::Invalid {
                var: FcmConfigVar::AppId,
                source: FcmIdentifierError::AppId
            })
        );
    }

    fn config_with_key_file(service_account_file: PathBuf) -> FcmConfig {
        FcmConfig {
            service_account_file,
            ..FcmConfig::from_vars(FULL)
                .expect("valid config")
                .expect("configured")
        }
    }

    #[tokio::test]
    async fn missing_or_unusable_key_files_fail_to_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = config_with_key_file(dir.path().join("absent.json"))
            .load_token_source()
            .await
            .expect_err("missing file");
        assert!(matches!(missing, FcmTokenSourceLoadError::Read { .. }));

        let not_json = dir.path().join("garbage.json");
        std::fs::write(&not_json, "not a key").expect("write garbage");
        let garbage = config_with_key_file(not_json)
            .load_token_source()
            .await
            .expect_err("garbage key");
        assert!(matches!(
            garbage,
            FcmTokenSourceLoadError::Key {
                source: FcmServiceAccountError::NotJson,
                ..
            }
        ));

        let bad_key = dir.path().join("bad-key.json");
        std::fs::write(
            &bad_key,
            serde_json::json!({
                "type": "service_account",
                "client_email": "sender@waddle-social.iam.gserviceaccount.com",
                "private_key": "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n",
            })
            .to_string(),
        )
        .expect("write bad key");
        let unusable = config_with_key_file(bad_key)
            .load_token_source()
            .await
            .expect_err("unusable key");
        assert!(matches!(
            unusable,
            FcmTokenSourceLoadError::Key {
                source: FcmServiceAccountError::PrivateKey(_),
                ..
            }
        ));
    }
}
