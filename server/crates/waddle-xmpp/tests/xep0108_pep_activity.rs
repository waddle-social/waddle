//! XEP-0108: User Activity — dedicated suite.

use minidom::Element;
use waddle_xmpp::pubsub::{AccessModel, PepHandler};
use waddle_xmpp::xep::xep0108::{
    build_activity_element, build_activity_retraction, is_activity_element, parse_activity_element,
    Activity, ActivityError, GeneralActivity, SpecificActivity, NS_ACTIVITY,
};

const ACTIVITY_NODE: &str = "http://jabber.org/protocol/activity";

#[test]
fn xep0108_well_known_node_is_supported() {
    assert!(PepHandler::is_well_known_node(ACTIVITY_NODE));
}

#[test]
fn xep0108_access_model_is_presence() {
    assert_eq!(
        PepHandler::default_access_model_for_node(ACTIVITY_NODE),
        AccessModel::Presence
    );
}

#[test]
fn xep0108_general_only_round_trip() {
    let a = Activity::new(GeneralActivity::Relaxing);
    let elem = build_activity_element(&a);
    let parsed = parse_activity_element(&elem).unwrap().unwrap();
    assert_eq!(parsed, a);
}

#[test]
fn xep0108_general_specific_text_round_trip() {
    let a = Activity::new(GeneralActivity::Working)
        .with_specific(SpecificActivity::new("coding").unwrap())
        .with_text("Hacking on Waddle");
    let elem = build_activity_element(&a);
    let parsed = parse_activity_element(&elem).unwrap().unwrap();
    assert_eq!(parsed, a);
}

#[test]
fn xep0108_empty_element_is_retraction() {
    let elem = build_activity_retraction();
    assert!(is_activity_element(&elem));
    assert!(parse_activity_element(&elem).unwrap().is_none());
}

#[test]
fn xep0108_foreign_children_do_not_cancel_retraction() {
    let elem = Element::builder("activity", NS_ACTIVITY)
        .append(Element::builder("ext", "urn:waddle:test").build())
        .build();
    assert!(parse_activity_element(&elem).unwrap().is_none());
}

#[test]
fn xep0108_unknown_general_rejected() {
    let elem = Element::builder("activity", NS_ACTIVITY)
        .append(Element::builder("flying", NS_ACTIVITY).build())
        .build();
    assert!(matches!(
        parse_activity_element(&elem),
        Err(ActivityError::UnknownGeneral(_))
    ));
}

#[test]
fn xep0108_specific_rejects_invalid_identifiers() {
    assert!(SpecificActivity::new("").is_none());
    assert!(SpecificActivity::new("Bad Name").is_none());
    assert!(SpecificActivity::new("UPPER").is_none());
    assert!(SpecificActivity::new("lower_ok").is_some());
}
