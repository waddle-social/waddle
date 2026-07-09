use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use sha2::{Digest, Sha256};

use super::model::*;

const TARGET_CONTRACT_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetContractDocument {
    schema_version: u32,
    resolved_jid_retention: RetentionPolicy,
    observed_identity_name_retention: RetentionPolicy,
    targets: Vec<TargetContractEntry>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "kebab-case")]
enum RetentionPolicy {
    Forbidden,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetContractEntry {
    slug: CapabilityTarget,
    jid_template: String,
    availability: TargetAvailability,
    collection_input: String,
    identities: Vec<TargetContractIdentity>,
    observation_policy: ObservationPolicy,
    required_features: Vec<String>,
    independently_optional_features: Vec<String>,
    runtime_feature_variants: Vec<Vec<String>>,
    claimable_features: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TargetContractIdentity {
    category: String,
    #[serde(rename = "type")]
    type_: String,
    #[serde(default)]
    name: Option<String>,
}

#[derive(Debug)]
pub(super) struct TargetExpectation {
    pub(super) identities: Vec<ObservedIdentity>,
    pub(super) observation_policy: ObservationPolicy,
    pub(super) required_features: BTreeSet<FeatureNamespace>,
    pub(super) independently_optional_features: BTreeSet<FeatureNamespace>,
    pub(super) runtime_feature_variants: Vec<BTreeSet<FeatureNamespace>>,
    pub(super) claimable_features: BTreeSet<FeatureNamespace>,
}

#[derive(Debug)]
pub(super) struct LoadedTargetContract {
    pub(super) sha256: Sha256Digest,
    pub(super) targets: BTreeMap<CapabilityTarget, TargetExpectation>,
}

impl LoadedTargetContract {
    pub(super) fn from_bytes(bytes: &[u8]) -> Result<Self, CapabilityEvidenceError> {
        let document: TargetContractDocument = serde_json::from_slice(bytes)?;
        if document.schema_version != TARGET_CONTRACT_SCHEMA_VERSION
            || document.resolved_jid_retention != RetentionPolicy::Forbidden
            || document.observed_identity_name_retention != RetentionPolicy::Forbidden
            || document.targets.len() != CapabilityTarget::ALL.len()
        {
            return Err(CapabilityEvidenceError::InvalidContract(
                "invalid schema or retention policy",
            ));
        }

        let mut targets = BTreeMap::new();
        for entry in document.targets {
            if entry.jid_template != entry.slug.jid_template()
                || entry.availability != entry.slug.availability()
                || entry.collection_input.is_empty()
                || entry.identities.is_empty()
                || entry.required_features.is_empty()
                || entry.claimable_features.is_empty()
            {
                return Err(CapabilityEvidenceError::InvalidContract(
                    "target metadata drifted",
                ));
            }
            let identities = entry
                .identities
                .into_iter()
                .map(|identity| {
                    if identity.name.as_ref().is_some_and(|name| {
                        name.is_empty() || name.len() > 64 || name.contains(['\r', '\n'])
                    }) {
                        return Err(CapabilityEvidenceError::InvalidContract(
                            "invalid static identity name",
                        ));
                    }
                    Ok(ObservedIdentity {
                        category: DiscoName::parse(identity.category)?,
                        type_: DiscoName::parse(identity.type_)?,
                    })
                })
                .collect::<Result<Vec<_>, _>>()?;
            if identities.iter().collect::<BTreeSet<_>>().len() != identities.len() {
                return Err(CapabilityEvidenceError::InvalidContract(
                    "duplicate target identity",
                ));
            }
            let required_feature_count = entry.required_features.len();
            let required_features = entry
                .required_features
                .into_iter()
                .map(FeatureNamespace::parse)
                .collect::<Result<BTreeSet<_>, _>>()?;
            if required_features.len() != required_feature_count {
                return Err(CapabilityEvidenceError::InvalidContract(
                    "duplicate required feature",
                ));
            }
            let independently_optional_feature_count = entry.independently_optional_features.len();
            let independently_optional_features = entry
                .independently_optional_features
                .into_iter()
                .map(FeatureNamespace::parse)
                .collect::<Result<BTreeSet<_>, _>>()?;
            if independently_optional_features.len() != independently_optional_feature_count {
                return Err(CapabilityEvidenceError::InvalidContract(
                    "duplicate independently optional feature",
                ));
            }
            let runtime_feature_variant_count = entry.runtime_feature_variants.len();
            let runtime_feature_variants = entry
                .runtime_feature_variants
                .into_iter()
                .map(|variant| {
                    let feature_count = variant.len();
                    let variant = variant
                        .into_iter()
                        .map(FeatureNamespace::parse)
                        .collect::<Result<BTreeSet<_>, _>>()?;
                    if variant.len() != feature_count || variant.is_empty() {
                        return Err(CapabilityEvidenceError::InvalidContract(
                            "runtime feature variants must be non-empty and unique",
                        ));
                    }
                    Ok(variant)
                })
                .collect::<Result<Vec<_>, CapabilityEvidenceError>>()?;
            if runtime_feature_variants
                .iter()
                .collect::<BTreeSet<_>>()
                .len()
                != runtime_feature_variant_count
            {
                return Err(CapabilityEvidenceError::InvalidContract(
                    "duplicate runtime feature variant",
                ));
            }
            let claimable_feature_count = entry.claimable_features.len();
            let claimable_features = entry
                .claimable_features
                .into_iter()
                .map(FeatureNamespace::parse)
                .collect::<Result<BTreeSet<_>, _>>()?;
            if claimable_features.len() != claimable_feature_count {
                return Err(CapabilityEvidenceError::InvalidContract(
                    "duplicate claimable feature",
                ));
            }
            let optional_overlaps_variant = runtime_feature_variants
                .iter()
                .any(|variant| !variant.is_disjoint(&independently_optional_features));
            let variant_union = runtime_feature_variants
                .iter()
                .flat_map(|variant| variant.iter().cloned())
                .collect::<BTreeSet<_>>();
            let variant_intersection = runtime_feature_variants.iter().skip(1).fold(
                runtime_feature_variants
                    .first()
                    .cloned()
                    .unwrap_or_default(),
                |common, variant| common.intersection(variant).cloned().collect(),
            );
            let mut expected_claimable = if runtime_feature_variants.is_empty() {
                required_features.clone()
            } else {
                variant_union
            };
            expected_claimable.extend(independently_optional_features.iter().cloned());
            let policy_is_valid = match entry.observation_policy {
                ObservationPolicy::ExactWhenAvailable => {
                    runtime_feature_variants.is_empty()
                        && independently_optional_features.is_empty()
                        && required_features == claimable_features
                }
                ObservationPolicy::RuntimeExtensible => runtime_feature_variants.is_empty(),
                ObservationPolicy::RuntimeDependent => {
                    !runtime_feature_variants.is_empty()
                        && required_features == variant_intersection
                }
            };
            if optional_overlaps_variant
                || !required_features.is_disjoint(&independently_optional_features)
                || expected_claimable != claimable_features
                || !policy_is_valid
            {
                return Err(CapabilityEvidenceError::InvalidContract(
                    "feature variants, optional features, and claimable union violate policy",
                ));
            }
            if targets
                .insert(
                    entry.slug,
                    TargetExpectation {
                        identities,
                        observation_policy: entry.observation_policy,
                        required_features,
                        independently_optional_features,
                        runtime_feature_variants,
                        claimable_features,
                    },
                )
                .is_some()
            {
                return Err(CapabilityEvidenceError::InvalidContract(
                    "duplicate target slug",
                ));
            }
        }
        let expected_targets = CapabilityTarget::ALL.into_iter().collect::<BTreeSet<_>>();
        let actual_targets = targets.keys().copied().collect::<BTreeSet<_>>();
        if actual_targets != expected_targets {
            return Err(CapabilityEvidenceError::InvalidContract(
                "target set drifted",
            ));
        }

        let digest = Sha256::digest(bytes)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        Ok(Self {
            sha256: Sha256Digest(digest),
            targets,
        })
    }
}
