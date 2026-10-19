//! Operator configuration for APNs delivery (#529).
//!
//! APNs is configured with four variables that only make sense
//! together:
//!
//! - `WADDLE_APNS_KEY_PATH`: path to the `.p8` token-signing key.
//! - `WADDLE_APNS_TEAM_ID`: Apple Developer team id (`iss` claim).
//! - `WADDLE_APNS_KEY_ID`: id of that key (`kid` header).
//! - `WADDLE_APNS_BUNDLE_ID`: the app's bundle id (`apns-topic`).
//!
//! Setting none leaves APNs disabled (Apple devices record
//! `apns-not-configured`). Setting some but not all is a startup error,
//! so a half-configured deployment never boots silently undeliverable.
//!
//! The APNs environment (production or sandbox host) is not a server
//! setting: every `register-device` submission must carry `prod` or
//! `sandbox`, and each device is sent to the host matching its own
//! registration.

use std::fmt;
use std::path::PathBuf;
use std::sync::Arc;

use waddle_xmpp::push::apns::{
    ApnsIdentifierError, ApnsKeyError, ApnsKeyId, ApnsTeamId, ApnsTopic, CachingApnsTokenSigner,
    SystemApnsClock,
};

/// One of the `WADDLE_APNS_*` variables.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApnsConfigVar {
    KeyPath,
    TeamId,
    KeyId,
    BundleId,
}

impl ApnsConfigVar {
    const ALL: [Self; 4] = [Self::KeyPath, Self::TeamId, Self::KeyId, Self::BundleId];

    pub fn name(self) -> &'static str {
        match self {
            Self::KeyPath => "WADDLE_APNS_KEY_PATH",
            Self::TeamId => "WADDLE_APNS_TEAM_ID",
            Self::KeyId => "WADDLE_APNS_KEY_ID",
            Self::BundleId => "WADDLE_APNS_BUNDLE_ID",
        }
    }

    fn from_name(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|var| var.name() == name)
    }
}

impl fmt::Display for ApnsConfigVar {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.name())
    }
}

fn var_list(vars: &[ApnsConfigVar]) -> String {
    vars.iter()
        .map(|var| var.name())
        .collect::<Vec<_>>()
        .join(", ")
}

/// Why the `WADDLE_APNS_*` configuration was rejected.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApnsConfigError {
    #[error("APNs configuration is incomplete: set all of WADDLE_APNS_KEY_PATH, WADDLE_APNS_TEAM_ID, WADDLE_APNS_KEY_ID and WADDLE_APNS_BUNDLE_ID, or none; missing {}", var_list(.missing))]
    Incomplete { missing: Vec<ApnsConfigVar> },
    #[error("{var} is invalid: {source}")]
    Invalid {
        var: ApnsConfigVar,
        #[source]
        source: ApnsIdentifierError,
    },
}

/// Why the configured `.p8` key could not be turned into a signer.
#[derive(Debug, thiserror::Error)]
pub enum ApnsSignerLoadError {
    #[error("failed to read WADDLE_APNS_KEY_PATH {path}: {source}")]
    Read {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("WADDLE_APNS_KEY_PATH {path} is not a usable APNs key: {source}")]
    Key {
        path: PathBuf,
        #[source]
        source: ApnsKeyError,
    },
}

/// Validated APNs configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApnsConfig {
    pub key_path: PathBuf,
    pub team_id: ApnsTeamId,
    pub key_id: ApnsKeyId,
    pub topic: ApnsTopic,
}

#[derive(Default)]
struct RawApnsVars {
    key_path: Option<String>,
    team_id: Option<String>,
    key_id: Option<String>,
    bundle_id: Option<String>,
}

impl RawApnsVars {
    fn slot(&mut self, var: ApnsConfigVar) -> &mut Option<String> {
        match var {
            ApnsConfigVar::KeyPath => &mut self.key_path,
            ApnsConfigVar::TeamId => &mut self.team_id,
            ApnsConfigVar::KeyId => &mut self.key_id,
            ApnsConfigVar::BundleId => &mut self.bundle_id,
        }
    }
}

impl ApnsConfig {
    /// `Ok(None)` when no `WADDLE_APNS_*` variable is set.
    pub fn from_env() -> Result<Option<Self>, ApnsConfigError> {
        Self::from_vars(std::env::vars())
    }

    /// Testable form of [`Self::from_env`]. Empty values count as unset.
    pub fn from_vars<I, K, V>(vars: I) -> Result<Option<Self>, ApnsConfigError>
    where
        I: IntoIterator<Item = (K, V)>,
        K: AsRef<str>,
        V: AsRef<str>,
    {
        let mut raw = RawApnsVars::default();
        for (key, value) in vars {
            let value = value.as_ref().trim();
            if value.is_empty() {
                continue;
            }
            if let Some(var) = ApnsConfigVar::from_name(key.as_ref()) {
                *raw.slot(var) = Some(value.to_owned());
            }
        }
        let RawApnsVars {
            key_path: Some(key_path),
            team_id: Some(team_id),
            key_id: Some(key_id),
            bundle_id: Some(bundle_id),
        } = raw
        else {
            let missing = ApnsConfigVar::ALL
                .into_iter()
                .filter(|var| raw.slot(*var).is_none())
                .collect::<Vec<_>>();
            return if missing.len() == ApnsConfigVar::ALL.len() {
                Ok(None)
            } else {
                Err(ApnsConfigError::Incomplete { missing })
            };
        };
        let invalid = |var| move |source| ApnsConfigError::Invalid { var, source };
        Ok(Some(Self {
            key_path: PathBuf::from(key_path),
            team_id: ApnsTeamId::parse(&team_id).map_err(invalid(ApnsConfigVar::TeamId))?,
            key_id: ApnsKeyId::parse(&key_id).map_err(invalid(ApnsConfigVar::KeyId))?,
            topic: ApnsTopic::parse(&bundle_id).map_err(invalid(ApnsConfigVar::BundleId))?,
        }))
    }

    /// Read the `.p8` key once and build the caching provider-token
    /// signer. Failing here fails boot: a configured-but-broken APNs
    /// key must not degrade silently.
    pub async fn load_token_signer(&self) -> Result<CachingApnsTokenSigner, ApnsSignerLoadError> {
        let pem =
            zeroize::Zeroizing::new(tokio::fs::read_to_string(&self.key_path).await.map_err(
                |source| ApnsSignerLoadError::Read {
                    path: self.key_path.clone(),
                    source,
                },
            )?);
        CachingApnsTokenSigner::from_pkcs8_pem(
            self.team_id.clone(),
            self.key_id.clone(),
            &pem,
            Arc::new(SystemApnsClock),
        )
        .map_err(|source| ApnsSignerLoadError::Key {
            path: self.key_path.clone(),
            source,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FULL: [(&str, &str); 4] = [
        ("WADDLE_APNS_KEY_PATH", "/run/secrets/apns.p8"),
        ("WADDLE_APNS_TEAM_ID", "TEAM123456"),
        ("WADDLE_APNS_KEY_ID", "KEY1234567"),
        ("WADDLE_APNS_BUNDLE_ID", "p4x.waddle.social"),
    ];

    #[test]
    fn no_apns_vars_disables_apns() {
        assert_eq!(
            ApnsConfig::from_vars([("WADDLE_MODE", "homeserver")]),
            Ok(None)
        );
    }

    #[test]
    fn empty_apns_vars_count_as_unset() {
        let empty = FULL.map(|(key, _)| (key, ""));
        assert_eq!(ApnsConfig::from_vars(empty), Ok(None));
    }

    #[test]
    fn all_apns_vars_produce_a_typed_config() {
        let config = ApnsConfig::from_vars(FULL)
            .expect("valid config")
            .expect("configured");
        assert_eq!(config.key_path, PathBuf::from("/run/secrets/apns.p8"));
        assert_eq!(config.team_id.as_str(), "TEAM123456");
        assert_eq!(config.key_id.as_str(), "KEY1234567");
        assert_eq!(config.topic.as_str(), "p4x.waddle.social");
    }

    #[test]
    fn partial_apns_vars_are_a_startup_error_naming_the_missing_ones() {
        let err = ApnsConfig::from_vars(FULL[..2].iter().copied()).expect_err("partial config");
        assert_eq!(
            err,
            ApnsConfigError::Incomplete {
                missing: vec![ApnsConfigVar::KeyId, ApnsConfigVar::BundleId]
            }
        );
        let rendered = err.to_string();
        assert!(rendered.contains("missing WADDLE_APNS_KEY_ID, WADDLE_APNS_BUNDLE_ID"));
    }

    #[test]
    fn a_single_apns_var_is_still_partial() {
        let err = ApnsConfig::from_vars([("WADDLE_APNS_BUNDLE_ID", "p4x.waddle.social")])
            .expect_err("partial config");
        assert!(matches!(err, ApnsConfigError::Incomplete { missing } if missing.len() == 3));
    }

    #[test]
    fn malformed_identifiers_are_typed_errors() {
        let mut vars = FULL;
        vars[1] = ("WADDLE_APNS_TEAM_ID", "short");
        assert_eq!(
            ApnsConfig::from_vars(vars),
            Err(ApnsConfigError::Invalid {
                var: ApnsConfigVar::TeamId,
                source: ApnsIdentifierError::TeamId
            })
        );
        let mut vars = FULL;
        vars[3] = ("WADDLE_APNS_BUNDLE_ID", "not a bundle");
        assert!(matches!(
            ApnsConfig::from_vars(vars),
            Err(ApnsConfigError::Invalid {
                var: ApnsConfigVar::BundleId,
                ..
            })
        ));
    }

    fn config_with_key_path(key_path: PathBuf) -> ApnsConfig {
        ApnsConfig {
            key_path,
            ..ApnsConfig::from_vars(FULL)
                .expect("valid config")
                .expect("configured")
        }
    }

    #[tokio::test]
    async fn loads_a_p8_key_from_disk() {
        use p256::pkcs8::EncodePrivateKey;
        use waddle_xmpp::push::apns::ApnsProviderTokenSource;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("AuthKey_KEY1234567.p8");
        let pem = p256::SecretKey::random(&mut p256::elliptic_curve::rand_core::OsRng)
            .to_pkcs8_pem(Default::default())
            .expect("pem");
        std::fs::write(&path, pem.as_bytes()).expect("write key");
        let signer = config_with_key_path(path)
            .load_token_signer()
            .await
            .expect("signer");
        assert!(signer.current().is_ok());
    }

    #[tokio::test]
    async fn missing_or_garbage_key_files_fail_to_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let missing = config_with_key_path(dir.path().join("absent.p8"))
            .load_token_signer()
            .await
            .expect_err("missing file");
        assert!(matches!(missing, ApnsSignerLoadError::Read { .. }));
        let garbage_path = dir.path().join("garbage.p8");
        std::fs::write(&garbage_path, "not a key").expect("write garbage");
        let garbage = config_with_key_path(garbage_path)
            .load_token_signer()
            .await
            .expect_err("garbage key");
        assert!(matches!(garbage, ApnsSignerLoadError::Key { .. }));
    }
}
