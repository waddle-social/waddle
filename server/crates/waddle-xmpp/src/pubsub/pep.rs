//! Server-facing re-exports for shared PEP helpers.

pub use waddle_xmpp_core::pubsub::{
    build_pep_identity, is_pep_request, is_pep_request_to, PepHandler,
};

use waddle_xmpp_core::disco::info::Feature;

/// Extended PEP features: core PEP features plus waddle-specific extensions
/// (currently: XEP-0430 Inbox advertisement).
pub fn pep_features() -> Vec<Feature> {
    let mut features = waddle_xmpp_core::pubsub::pep_features();
    features.push(Feature::new(crate::xep::xep0430::NS_INBOX));
    features
}
