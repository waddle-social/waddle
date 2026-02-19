//! XEP-0384: OMEMO compatibility dedicated suite.

use waddle_xmpp::disco::Feature;
use waddle_xmpp::pubsub::pep::pep_features;
use waddle_xmpp::pubsub::{AccessModel, PepHandler};

const OMEMO_NODE: &str = "eu.siacs.conversations.axolotl.devicelist";
const OMEMO_FALLBACK_FEATURE: &str = "eu.siacs.conversations.axolotl.whitelisted";

#[test]
fn xep0384_omemo_nodes_are_supported() {
    assert!(PepHandler::is_well_known_node(OMEMO_NODE));
}

#[test]
fn xep0384_omemo_node_access_model_is_open() {
    assert_eq!(
        PepHandler::default_access_model_for_node(OMEMO_NODE),
        AccessModel::Open
    );
}

#[test]
fn xep0384_fallback_feature_is_advertised() {
    let features = pep_features();
    assert!(features.contains(&Feature::new(OMEMO_FALLBACK_FEATURE)));
}

#[test]
fn xep0384_unknown_axolotl_variant_is_rejected() {
    assert!(!PepHandler::is_well_known_node(
        "eu.siacs.conversations.whitelist"
    ));
}
