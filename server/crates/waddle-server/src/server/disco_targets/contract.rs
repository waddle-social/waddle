use serde::Serialize;

use super::{
    claimable_target_features, independently_optional_target_features, required_target_features,
    runtime_target_feature_variants, target_identity_contracts, DiscoIdentity, DiscoTarget,
    DiscoTargetAvailability,
};

const TARGET_CONTRACT_SCHEMA_VERSION: u32 = 2;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum DiscoCollectionInput {
    XmppDomain,
    ConfiguredMucDomain,
    ConfiguredSpacesDomain,
    ConfiguredCalls,
    RepresentativeMucRoom,
    AuthenticatedSelf,
}

const fn collection_input(target: DiscoTarget) -> DiscoCollectionInput {
    match target {
        DiscoTarget::Server
        | DiscoTarget::UploadService
        | DiscoTarget::CommunityService
        | DiscoTarget::ExtensionsService
        | DiscoTarget::PushService => DiscoCollectionInput::XmppDomain,
        DiscoTarget::MucService => DiscoCollectionInput::ConfiguredMucDomain,
        DiscoTarget::SpacesService => DiscoCollectionInput::ConfiguredSpacesDomain,
        DiscoTarget::CallsMixer => DiscoCollectionInput::ConfiguredCalls,
        DiscoTarget::RepresentativeMucRoom => DiscoCollectionInput::RepresentativeMucRoom,
        DiscoTarget::AuthenticatedSelf => DiscoCollectionInput::AuthenticatedSelf,
    }
}

#[derive(Debug, Serialize)]
pub struct DiscoTargetContract {
    schema_version: u32,
    resolved_jid_retention: &'static str,
    observed_identity_name_retention: &'static str,
    targets: Vec<DiscoTargetContractEntry>,
}

#[derive(Debug, Serialize)]
struct DiscoTargetContractEntry {
    slug: &'static str,
    jid_template: &'static str,
    availability: DiscoTargetAvailability,
    collection_input: DiscoCollectionInput,
    identities: &'static [DiscoIdentity],
    observation_policy: DiscoObservationPolicy,
    required_features: Vec<String>,
    independently_optional_features: Vec<String>,
    runtime_feature_variants: Vec<Vec<String>>,
    claimable_features: Vec<String>,
}

/// How a collector compares a live vector with target-local exact variants.
/// Curated extensions vary independently; every other runtime-dependent
/// feature must belong to one complete generated server/room configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "kebab-case")]
enum DiscoObservationPolicy {
    ExactWhenAvailable,
    RuntimeExtensible,
    RuntimeDependent,
}

const fn observation_policy(target: DiscoTarget) -> DiscoObservationPolicy {
    match target {
        DiscoTarget::Server | DiscoTarget::RepresentativeMucRoom => {
            DiscoObservationPolicy::RuntimeDependent
        }
        DiscoTarget::MucService | DiscoTarget::ExtensionsService => {
            DiscoObservationPolicy::RuntimeExtensible
        }
        DiscoTarget::UploadService
        | DiscoTarget::SpacesService
        | DiscoTarget::CommunityService
        | DiscoTarget::PushService
        | DiscoTarget::CallsMixer
        | DiscoTarget::AuthenticatedSelf => DiscoObservationPolicy::ExactWhenAvailable,
    }
}

pub fn target_contract() -> DiscoTargetContract {
    DiscoTargetContract {
        schema_version: TARGET_CONTRACT_SCHEMA_VERSION,
        resolved_jid_retention: "forbidden",
        observed_identity_name_retention: "forbidden",
        targets: DiscoTarget::ALL
            .into_iter()
            .map(|target| DiscoTargetContractEntry {
                slug: target.slug(),
                jid_template: target.jid_template(),
                availability: target.availability(),
                collection_input: collection_input(target),
                identities: target_identity_contracts(target),
                observation_policy: observation_policy(target),
                required_features: required_target_features(target)
                    .into_iter()
                    .map(|feature| feature.0)
                    .collect(),
                independently_optional_features: independently_optional_target_features(target)
                    .into_iter()
                    .map(|feature| feature.0)
                    .collect(),
                runtime_feature_variants: runtime_target_feature_variants(target)
                    .into_iter()
                    .map(|variant| variant.into_iter().map(|feature| feature.0).collect())
                    .collect(),
                claimable_features: claimable_target_features(target)
                    .into_iter()
                    .map(|feature| feature.0)
                    .collect(),
            })
            .collect(),
    }
}

pub fn target_contract_json() -> Result<String, serde_json::Error> {
    serde_json::to_string_pretty(&target_contract())
}
