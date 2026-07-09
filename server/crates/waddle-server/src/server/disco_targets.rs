//! Typed source of truth for live XEP-0030 discovery targets.
//!
//! The target contract deliberately contains templates, never resolved user or
//! room JIDs. A collector may resolve the two dynamic inputs for the duration
//! of a run, but evidence retains only the stable target slug, identities, and
//! observed features.

mod contract;
mod features;
mod identities;

pub use contract::{
    target_contract, target_contract_json, DiscoCollectionInput, DiscoTargetContract,
};
pub(crate) use features::calls_available;
use features::independently_optional_target_features;
pub use features::{
    authenticated_self_target_features, calls_mixer_target_features, claimable_target_features,
    extensions_service_target_features, manifest_target_features, muc_room_target_features,
    muc_service_target_features, required_target_features, runtime_target_feature_variants,
    server_target_features, MucRoomFeatureOptions, RuntimeFeatureOptions,
};
#[cfg(test)]
use features::{
    calls_available_from_parts, curated_extension_features, CURATED_EXTENSION_NAMESPACES,
};
pub use identities::{
    target_identities, target_identities_with_name, target_identity_contracts, DiscoIdentity,
    DiscoIdentityCategory, DiscoIdentityName, DiscoIdentityType,
};
pub use waddle_xmpp_core::disco_target::{DiscoTarget, DiscoTargetAvailability};

#[cfg(test)]
mod tests;
