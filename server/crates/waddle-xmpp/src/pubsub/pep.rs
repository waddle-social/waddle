//! Server-facing re-exports for shared PEP helpers.

use crate::xep::xep0430::NS_INBOX;
pub use waddle_xmpp_core::pubsub::{
    build_pep_identity, is_pep_request, is_pep_request_to, PepHandler, PEP_NODE_AVATAR_DATA,
    PEP_NODE_AVATAR_METADATA, PEP_NODE_BOOKMARKS,
};

use waddle_xmpp_core::disco::info::Feature;

/// Extended PEP features: core PEP features plus Waddle-specific extensions.
pub fn pep_features() -> Vec<Feature> {
    let mut features = waddle_xmpp_core::pubsub::pep_features();
    features.push(Feature::new(NS_INBOX));
    features
}
