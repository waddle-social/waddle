use std::collections::BTreeSet;
use std::future::Future;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use jid::BareJid;
use serde::Serialize;

use crate::DiscoInfoResult;

use super::contract::LoadedTargetContract;
use super::model::*;

#[derive(Debug, Clone)]
pub(super) struct TargetInputs {
    pub(super) xmpp_domain: BareJid,
    pub(super) muc_domain: BareJid,
    pub(super) spaces_domain: BareJid,
    pub(super) account: BareJid,
    pub(super) representative_muc_room: Option<BareJid>,
    pub(super) calls_configured: bool,
}

impl TargetInputs {
    pub(super) fn validate(&self) -> Result<(), CapabilityEvidenceError> {
        if self.xmpp_domain.node().is_some()
            || self.muc_domain.node().is_some()
            || self.spaces_domain.node().is_some()
        {
            return Err(CapabilityEvidenceError::InvalidArgument(
                "XMPP service domains must not contain a localpart",
            ));
        }
        if self.account.node().is_none() || self.account.domain() != self.xmpp_domain.domain() {
            return Err(CapabilityEvidenceError::InvalidArgument(
                "authenticated account domain must match the XMPP domain",
            ));
        }
        if self
            .representative_muc_room
            .as_ref()
            .is_some_and(|room| room.node().is_none() || room.domain() != self.muc_domain.domain())
        {
            return Err(CapabilityEvidenceError::InvalidArgument(
                "representative room must belong to the configured MUC domain",
            ));
        }
        Ok(())
    }

    fn resolve(
        &self,
        target: CapabilityTarget,
    ) -> Result<TargetResolution, CapabilityEvidenceError> {
        let derived = |prefix: &str| {
            BareJid::from_str(&format!("{prefix}.{}", self.xmpp_domain)).map_err(|_| {
                CapabilityEvidenceError::InvalidArgument("derived component JID is invalid")
            })
        };
        match target {
            CapabilityTarget::Server => Ok(TargetResolution::Query(self.xmpp_domain.clone())),
            CapabilityTarget::MucService => Ok(TargetResolution::Query(self.muc_domain.clone())),
            CapabilityTarget::UploadService => Ok(TargetResolution::Query(derived("upload")?)),
            CapabilityTarget::SpacesService => {
                Ok(TargetResolution::Query(self.spaces_domain.clone()))
            }
            CapabilityTarget::CommunityService => {
                Ok(TargetResolution::Query(derived("community")?))
            }
            CapabilityTarget::ExtensionsService => {
                Ok(TargetResolution::Query(derived("extensions")?))
            }
            CapabilityTarget::PushService => Ok(TargetResolution::Query(derived("push")?)),
            CapabilityTarget::CallsMixer if self.calls_configured => {
                Ok(TargetResolution::Query(derived("calls")?))
            }
            CapabilityTarget::CallsMixer => Ok(TargetResolution::Skip(SkipReason::NotConfigured)),
            CapabilityTarget::RepresentativeMucRoom => self
                .representative_muc_room
                .clone()
                .map(TargetResolution::Query)
                .map_or(
                    Ok(TargetResolution::Skip(SkipReason::NoRepresentativeEntity)),
                    Ok,
                ),
            CapabilityTarget::AuthenticatedSelf => {
                Ok(TargetResolution::Query(self.account.clone()))
            }
        }
    }
}

enum TargetResolution {
    Query(BareJid),
    Skip(SkipReason),
}

#[derive(Debug, Clone)]
pub(super) struct CollectionWindow {
    pub(super) start: DateTime<Utc>,
    pub(super) end: DateTime<Utc>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(super) struct EvidenceWindow {
    pub(super) start: UtcInstant,
    pub(super) end: UtcInstant,
}

impl From<&CollectionWindow> for EvidenceWindow {
    fn from(value: &CollectionWindow) -> Self {
        Self {
            start: UtcInstant::from_datetime(value.start),
            end: UtcInstant::from_datetime(value.end),
        }
    }
}

impl CollectionWindow {
    fn contains(&self, instant: DateTime<Utc>) -> bool {
        instant >= self.start && instant <= self.end
    }
}

#[derive(Debug, Clone)]
pub(super) struct CollectionProvenance {
    pub(super) server_commit: GitCommit,
    pub(super) deployment_scope: DeploymentScope,
    pub(super) window: CollectionWindow,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QueryFailure {
    Failed,
    TimedOut,
}

pub(super) async fn collect_live_disco_with<Q, Fut>(
    contract: &LoadedTargetContract,
    target_inputs: &TargetInputs,
    provenance: &CollectionProvenance,
    mut query: Q,
    captured_at: impl FnOnce() -> DateTime<Utc>,
) -> Result<LiveDiscoArtifact, CapabilityEvidenceError>
where
    Q: FnMut(CapabilityTarget, BareJid) -> Fut,
    Fut: Future<Output = Result<DiscoInfoResult, QueryFailure>>,
{
    target_inputs.validate()?;
    let mut entities = Vec::new();
    let mut skipped_targets = Vec::new();
    for target in CapabilityTarget::ALL {
        match target_inputs.resolve(target)? {
            TargetResolution::Skip(reason) => {
                skipped_targets.push(SkippedTarget { target, reason });
            }
            TargetResolution::Query(jid) => {
                let result = query(target, jid).await.map_err(|failure| match failure {
                    QueryFailure::Failed => CapabilityEvidenceError::QueryFailed { target },
                    QueryFailure::TimedOut => CapabilityEvidenceError::QueryTimedOut { target },
                })?;
                entities.push(sanitize_observation(contract, target, result)?);
            }
        }
    }
    let captured_at = captured_at();
    if !provenance.window.contains(captured_at) {
        return Err(CapabilityEvidenceError::CapturedOutsideWindow);
    }
    Ok(LiveDiscoArtifact {
        schema_version: SCHEMA_VERSION,
        artifact_role: ArtifactRole::LiveDiscoExport,
        evidence_kind: EvidenceKind::Gate0CapabilityLiveDisco,
        status: CollectionStatus::Collected,
        server_commit: provenance.server_commit.clone(),
        captured_at: UtcInstant::from_datetime(captured_at),
        window: EvidenceWindow::from(&provenance.window),
        deployment_scope: provenance.deployment_scope.clone(),
        target_contract_sha256: contract.sha256.clone(),
        entities,
        skipped_targets,
    })
}

pub(super) fn sanitize_observation(
    contract: &LoadedTargetContract,
    target: CapabilityTarget,
    result: DiscoInfoResult,
) -> Result<ObservedEntity, CapabilityEvidenceError> {
    let identities = result
        .identities
        .into_iter()
        .map(|identity| {
            Ok(ObservedIdentity {
                category: DiscoName::parse(identity.category)?,
                type_: DiscoName::parse(identity.identity_type)?,
            })
        })
        .collect::<Result<Vec<_>, CapabilityEvidenceError>>()?;
    let mut features = BTreeSet::new();
    for feature in result.features {
        if !features.insert(FeatureNamespace::parse(feature)?) {
            return Err(CapabilityEvidenceError::InvalidObservation { target });
        }
    }
    let expectation =
        contract
            .targets
            .get(&target)
            .ok_or(CapabilityEvidenceError::InvalidContract(
                "missing target expectation",
            ))?;
    let base_features = features
        .difference(&expectation.independently_optional_features)
        .cloned()
        .collect::<BTreeSet<_>>();
    let features_are_valid = features.is_subset(&expectation.claimable_features)
        && match expectation.observation_policy {
            ObservationPolicy::ExactWhenAvailable | ObservationPolicy::RuntimeExtensible => {
                base_features == expectation.required_features
            }
            ObservationPolicy::RuntimeDependent => expectation
                .runtime_feature_variants
                .contains(&base_features),
        };
    if identities != expectation.identities || !features_are_valid {
        return Err(CapabilityEvidenceError::InvalidObservation { target });
    }
    Ok(ObservedEntity {
        target,
        identities,
        features: features.into_iter().collect(),
    })
}
