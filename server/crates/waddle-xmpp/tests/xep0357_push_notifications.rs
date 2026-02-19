//! XEP-0357: Push Notifications compatibility dedicated suite.

use waddle_xmpp::disco::{server_features, Feature};
use waddle_xmpp::pubsub::pep::pep_features;

const PUSH_FEATURE: &str = "urn:xmpp:push:0";

#[test]
fn xep0357_pep_advertises_push_feature() {
    let features = pep_features();
    assert!(features.contains(&Feature::new(PUSH_FEATURE)));
}

#[test]
fn xep0357_push_feature_not_leaked_to_server_root_disco() {
    let features = server_features();
    assert!(!features.contains(&Feature::new(PUSH_FEATURE)));
}

#[test]
fn xep0357_push_feature_is_unique_in_pep_features() {
    let features = pep_features();
    let count = features.iter().filter(|f| f.0 == PUSH_FEATURE).count();
    assert_eq!(count, 1);
}
