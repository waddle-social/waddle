use chrono::{DateTime, SecondsFormat, Utc};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use url::Url;
pub use waddle_xmpp_core::disco_target::DiscoTarget as CapabilityTarget;
pub(super) use waddle_xmpp_core::disco_target::DiscoTargetAvailability as TargetAvailability;

use super::collect::EvidenceWindow;

pub(super) const SCHEMA_VERSION: u32 = 1;
pub(super) const DEFAULT_IDENTITY_METRIC: &str = "waddle_build_info";
pub(super) const DEFAULT_TARGET_SIGNAL_ID: &str = "server-deployment-identity-targets";
pub(super) const DEFAULT_IDENTITY_LOOKBACK_SECONDS: u32 = 3_600;

#[derive(Debug, Error)]
pub enum CapabilityEvidenceError {
    #[error("invalid collector argument: {0}")]
    InvalidArgument(&'static str),
    #[error("invalid disco target contract: {0}")]
    InvalidContract(&'static str),
    #[error("access token environment variable is missing or not valid UTF-8")]
    MissingAccessToken,
    #[error("native XMPP connection failed")]
    ConnectionFailed,
    #[error("disco query for {target} failed")]
    QueryFailed { target: CapabilityTarget },
    #[error("disco query for {target} timed out")]
    QueryTimedOut { target: CapabilityTarget },
    #[error("disco observation for {target} violates the target contract")]
    InvalidObservation { target: CapabilityTarget },
    #[error("capture completed outside the fixed evidence window")]
    CapturedOutsideWindow,
    #[error("output already exists")]
    OutputExists,
    #[error("collector filesystem operation failed")]
    Io(#[from] std::io::Error),
    #[error("collector JSON operation failed")]
    Json(#[from] serde_json::Error),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ObservationPolicy {
    ExactWhenAvailable,
    RuntimeExtensible,
    RuntimeDependent,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub(super) struct DiscoName(pub(super) String);

impl DiscoName {
    pub(super) fn parse(value: String) -> Result<Self, CapabilityEvidenceError> {
        let valid = !value.is_empty()
            && value.len() <= 64
            && value
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
            && value
                .as_bytes()
                .first()
                .is_some_and(u8::is_ascii_alphanumeric)
            && value
                .as_bytes()
                .last()
                .is_some_and(u8::is_ascii_alphanumeric);
        valid
            .then_some(Self(value))
            .ok_or(CapabilityEvidenceError::InvalidContract(
                "invalid identity name",
            ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(transparent)]
pub(super) struct FeatureNamespace(pub(super) String);

impl FeatureNamespace {
    pub(super) fn parse(value: String) -> Result<Self, CapabilityEvidenceError> {
        if value.is_empty()
            || value.len() > 256
            || value
                .chars()
                .any(|character| character.is_whitespace() || "?&=@".contains(character))
        {
            return Err(CapabilityEvidenceError::InvalidContract(
                "invalid feature namespace",
            ));
        }
        let approved_waddle = value.strip_prefix("urn:waddle:").is_some_and(|suffix| {
            let segments = suffix.split(':').collect::<Vec<_>>();
            let Some((version, names)) = segments.split_last() else {
                return false;
            };
            !names.is_empty()
                && !version.is_empty()
                && version.len() <= 3
                && version.bytes().all(|byte| byte.is_ascii_digit())
                && names.iter().all(|segment| {
                    !segment.is_empty()
                        && segment.len() <= 32
                        && segment.as_bytes()[0].is_ascii_lowercase()
                        && segment.bytes().all(|byte| {
                            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-'
                        })
                })
        });
        let approved_urn = value.starts_with("urn:xmpp:")
            || value.starts_with("urn:ietf:")
            || value.starts_with("jabber:")
            || value.starts_with("storage:")
            || value == "vcard-temp"
            || value.starts_with("vcard-temp:")
            || value == "msgoffline"
            || value.strip_prefix("muc_").is_some_and(|suffix| {
                !suffix.is_empty()
                    && suffix
                        .bytes()
                        .all(|byte| byte.is_ascii_lowercase() || byte == b'_')
            });
        let approved_url = Url::parse(&value).is_ok_and(|url| {
            let registered_jabber_namespace = matches!(url.scheme(), "http" | "https")
                && matches!(url.host_str(), Some("jabber.org" | "www.jabber.org"));
            let pinned_isr_namespace = url.scheme() == "https"
                && url.host_str() == Some("xmpp.org")
                && url.path() == "/extensions/isr/0"
                && url.fragment().is_none();
            (registered_jabber_namespace || pinned_isr_namespace)
                && url.username().is_empty()
                && url.password().is_none()
                && url.query().is_none()
        });
        (approved_waddle || approved_urn || approved_url)
            .then_some(Self(value))
            .ok_or(CapabilityEvidenceError::InvalidContract(
                "unapproved feature namespace",
            ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize)]
pub(super) struct ObservedIdentity {
    pub(super) category: DiscoName,
    #[serde(rename = "type")]
    pub(super) type_: DiscoName,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct ObservedEntity {
    pub(super) target: CapabilityTarget,
    pub(super) identities: Vec<ObservedIdentity>,
    pub(super) features: Vec<FeatureNamespace>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum SkipReason {
    NotConfigured,
    NoRepresentativeEntity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct SkippedTarget {
    pub(super) target: CapabilityTarget,
    pub(super) reason: SkipReason,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(super) struct GitCommit(pub(super) String);

impl GitCommit {
    pub(super) fn parse(value: String) -> Result<Self, CapabilityEvidenceError> {
        (value.len() == 40
            && value
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)))
        .then_some(Self(value))
        .ok_or(CapabilityEvidenceError::InvalidArgument(
            "server commit must be a full lowercase Git SHA",
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(super) struct Sha256Digest(pub(super) String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(super) struct ScopeLabel(String);

impl ScopeLabel {
    pub(super) fn parse(value: String) -> Result<Self, CapabilityEvidenceError> {
        let bytes = value.as_bytes();
        let valid = value != "unknown"
            && !value.is_empty()
            && value.len() <= 64
            && bytes.first().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.last().is_some_and(u8::is_ascii_alphanumeric)
            && bytes.iter().all(|byte| {
                byte.is_ascii_lowercase()
                    || byte.is_ascii_digit()
                    || matches!(byte, b'.' | b'_' | b'-')
            });
        valid
            .then_some(Self(value))
            .ok_or(CapabilityEvidenceError::InvalidArgument(
                "deployment scope labels must be bounded lowercase values",
            ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(super) struct MetricName(String);

impl MetricName {
    pub(super) fn parse(value: String) -> Result<Self, CapabilityEvidenceError> {
        let mut bytes = value.bytes();
        let first = bytes.next();
        let valid = first
            .is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b':'))
            && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b':'));
        valid
            .then_some(Self(value))
            .ok_or(CapabilityEvidenceError::InvalidArgument(
                "identity metric must be a Prometheus metric name",
            ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(super) struct SignalId(String);

impl SignalId {
    pub(super) fn parse(value: String) -> Result<Self, CapabilityEvidenceError> {
        let mut bytes = value.bytes();
        let valid = bytes.next().is_some_and(|byte| byte.is_ascii_lowercase())
            && bytes.all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-');
        valid
            .then_some(Self(value))
            .ok_or(CapabilityEvidenceError::InvalidArgument(
                "target signal id must be lowercase kebab-case",
            ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct DeploymentScope {
    pub(super) job: ScopeLabel,
    pub(super) environment: ScopeLabel,
    pub(super) cluster: ScopeLabel,
    pub(super) namespace: ScopeLabel,
    pub(super) expected_replicas: u32,
    pub(super) identity_metric: MetricName,
    pub(super) target_signal_id: SignalId,
    pub(super) identity_lookback_seconds: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(transparent)]
pub(super) struct UtcInstant(String);

impl UtcInstant {
    pub(super) fn from_datetime(value: DateTime<Utc>) -> Self {
        Self(value.to_rfc3339_opts(SecondsFormat::Secs, true))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum ArtifactRole {
    LiveDiscoExport,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum EvidenceKind {
    Gate0CapabilityLiveDisco,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum CollectionStatus {
    Collected,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct LiveDiscoArtifact {
    pub(super) schema_version: u32,
    pub(super) artifact_role: ArtifactRole,
    pub(super) evidence_kind: EvidenceKind,
    pub(super) status: CollectionStatus,
    pub(super) server_commit: GitCommit,
    pub(super) captured_at: UtcInstant,
    pub(super) window: EvidenceWindow,
    pub(super) deployment_scope: DeploymentScope,
    pub(super) target_contract_sha256: Sha256Digest,
    pub(super) entities: Vec<ObservedEntity>,
    pub(super) skipped_targets: Vec<SkippedTarget>,
}

impl LiveDiscoArtifact {
    pub(super) fn to_pretty_json(&self) -> Result<String, CapabilityEvidenceError> {
        Ok(format!("{}\n", serde_json::to_string_pretty(self)?))
    }
}
