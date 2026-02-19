//! XEP-0490 compatibility signaling dedicated suite.

use waddle_xmpp::disco::{server_features, Feature};
use waddle_xmpp::pubsub::pep::pep_features;

const CONFIG_NODE_MAX_FEATURE: &str = "http://jabber.org/protocol/pubsub#config-node-max";

#[test]
fn xep0490_pep_advertises_config_node_max() {
    let features = pep_features();
    assert!(features.contains(&Feature::new(CONFIG_NODE_MAX_FEATURE)));
}

#[test]
fn xep0490_config_node_max_not_in_server_root_disco() {
    let features = server_features();
    assert!(!features.contains(&Feature::new(CONFIG_NODE_MAX_FEATURE)));
}

#[test]
fn xep0490_config_node_max_is_unique_in_pep_features() {
    let features = pep_features();
    let count = features
        .iter()
        .filter(|f| f.0 == CONFIG_NODE_MAX_FEATURE)
        .count();
    assert_eq!(count, 1);
}
